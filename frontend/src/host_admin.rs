//! Host view facade. Session/account/project transitions invalidate every async result.
use crate::{
    backend::Api,
    state::{auth::AuthState, chat::ChatState, projects::ProjectsState},
};
use leptos::prelude::*;
use openwebide_core::host_admin::{
    HostConnection, HostEnvironment, HostInputKeyRequest, HostOperation, HostRequest, HostResponse,
};
#[derive(Clone, Copy, PartialEq, Eq)]
struct Scope {
    account: u64,
    session: Option<i64>,
    project: Option<i64>,
}
#[derive(Clone, Copy)]
pub struct HostState {
    pub updates: crate::host_updates::UpdateState,
    pub connection: RwSignal<Option<HostConnection>>,
    pub environment: RwSignal<Option<HostEnvironment>>,
    pub operations: RwSignal<Vec<HostOperation>>,
    pub error: RwSignal<Option<String>>,
    pub busy: RwSignal<bool>,
    pub open: RwSignal<bool>,
    generation: RwSignal<u64>,
    connection_generation: RwSignal<u64>,
    api: Api,
    auth: AuthState,
    chat: ChatState,
    projects: ProjectsState,
}
impl HostState {
    pub fn new(api: Api, auth: AuthState, chat: ChatState, projects: ProjectsState) -> Self {
        let state = Self {
            updates: crate::host_updates::UpdateState::new(api, auth),
            connection: RwSignal::new(None),
            environment: RwSignal::new(None),
            operations: RwSignal::new(Vec::new()),
            error: RwSignal::new(None),
            busy: RwSignal::new(false),
            open: RwSignal::new(false),
            generation: RwSignal::new(0),
            connection_generation: RwSignal::new(0),
            api,
            auth,
            chat,
            projects,
        };
        Effect::new(move |previous: Option<Scope>| {
            let scope = Scope {
                account: auth.generation.get(),
                session: chat.active_session.get(),
                project: projects.active_project.get(),
            };
            if previous.is_some_and(|previous| previous != scope) {
                state.generation.update(|generation| *generation += 1);
                state.environment.set(None);
                state.operations.set(Vec::new());
                state.error.set(None);
                state.busy.set(false);
                state.open.set(false);
            }
            scope
        });
        Effect::new(move || {
            auth.generation.track();
            state.connection.set(None);
        });
        state
    }
    fn scope(self) -> Scope {
        Scope {
            account: self.auth.generation.get_untracked(),
            session: self.chat.active_session.get_untracked(),
            project: self.projects.active_project.get_untracked(),
        }
    }
    fn current(self, scope: Scope, generation: u64) -> bool {
        self.auth.generation.try_get_untracked().is_some()
            && self.scope() == scope
            && self.generation.get_untracked() == generation
    }
    fn begin(self) -> (Scope, u64) {
        self.generation.update(|generation| *generation += 1);
        self.busy.set(true);
        self.error.set(None);
        (self.scope(), self.generation.get_untracked())
    }
    pub fn load_connection(self) {
        let epoch = self.auth.generation.get_untracked();
        self.connection_generation
            .update(|generation| *generation += 1);
        let generation = self.connection_generation.get_untracked();
        let api = self.api.get_value();
        wasm_bindgen_futures::spawn_local(async move {
            let result = api.host_connection().await;
            if self.auth.generation.try_get_untracked() != Some(epoch)
                || self.connection_generation.try_get_untracked() != Some(generation)
            {
                return;
            }
            match result {
                Ok(connection) => self.connection.set(Some(connection)),
                Err(error) => self.error.set(Some(error)),
            }
        });
    }
    pub fn configured(self) -> bool {
        self.connection.with_untracked(|connection| {
            connection
                .as_ref()
                .is_some_and(HostConnection::is_configured)
        })
    }
    pub fn save_connection(self, connection: HostConnection) {
        self.connection_generation
            .update(|generation| *generation += 1);
        let (scope, generation) = self.begin();
        let api = self.api.get_value();
        wasm_bindgen_futures::spawn_local(async move {
            let result = api.save_host_connection(&connection).await;
            if !self.current(scope, generation) {
                return;
            }
            match result {
                Ok(connection) => {
                    self.connection.set(Some(connection));
                    self.environment.set(None);
                }
                Err(error) => self.error.set(Some(error)),
            }
            self.busy.set(false);
        });
    }
    pub fn test_connection(self) {
        let (scope, generation) = self.begin();
        let api = self.api.get_value();
        wasm_bindgen_futures::spawn_local(async move {
            let result = api.probe_host_connection().await;
            if !self.current(scope, generation) {
                return;
            }
            match result {
                Ok(environment) => self.environment.set(Some(environment)),
                Err(error) => self.error.set(Some(error)),
            }
            self.busy.set(false);
        });
    }
    pub fn refresh(self, inspect: bool) {
        let scope = self.scope();
        let Some(session) = scope.session.filter(|_| scope.project.is_none()) else {
            return;
        };
        if self.busy.get_untracked() || !self.configured() {
            return;
        }
        self.generation.update(|generation| *generation += 1);
        self.busy.set(true);
        let generation = self.generation.get_untracked();
        let api = self.api.get_value();
        wasm_bindgen_futures::spawn_local(async move {
            if inspect {
                let result = api.host_view(session, &HostRequest::Inspect).await;
                if !self.current(scope, generation) {
                    return;
                }
                match result {
                    Ok(HostResponse::Environment(environment)) => {
                        self.error.set(None);
                        self.environment.set(Some(environment));
                    }
                    Ok(_) => self
                        .error
                        .set(Some("Unexpected host environment response".into())),
                    Err(error) => self.error.set(Some(error)),
                }
            }
            let result = api.host_view(session, &HostRequest::Operations).await;
            if !self.current(scope, generation) {
                return;
            }
            match result {
                Ok(HostResponse::Operations(operations)) => self.operations.set(operations),
                Ok(_) => self
                    .error
                    .set(Some("Unexpected host operation response".into())),
                Err(error) => self.error.set(Some(error)),
            }
            self.busy.set(false);
        });
    }
    pub fn reply(self, operation: i64, token: String, data: String) {
        let scope = self.scope();
        let Some(session) = scope.session.filter(|_| scope.project.is_none()) else {
            return;
        };
        let Some(user) = self
            .auth
            .user
            .with_untracked(|value| value.as_ref().map(|user| user.id.get()))
        else {
            return;
        };
        let api = self.api.get_value();
        let prompt = HostInputKeyRequest { operation, token };
        wasm_bindgen_futures::spawn_local(async move {
            let current = || {
                self.auth.generation.try_get_untracked().is_some()
                    && self.scope() == scope
                    && self
                        .operations
                        .try_with_untracked(|operations| {
                            operations.iter().any(|operation| {
                                operation.id == prompt.operation
                                    && operation.input_token.as_deref()
                                        == Some(prompt.token.as_str())
                            })
                        })
                        .unwrap_or(false)
            };
            let result = async {
                if !current() {
                    return Ok(());
                }
                let key = api.host_input_key(session, &prompt).await?;
                if !current() {
                    return Ok(());
                }
                let mut entropy = [0_u8; 32];
                web_sys::window()
                    .ok_or("Private terminal encryption unavailable")?
                    .crypto()
                    .map_err(|_| "Private terminal encryption unavailable")?
                    .get_random_values_with_u8_array(&mut entropy)
                    .map_err(|_| "Private terminal randomness unavailable")?;
                let encrypted = openwebide_core::host_admin::private_input::encrypt_reply(
                    &key, user, session, &prompt, &data, entropy,
                )?;
                if !current() {
                    return Ok(());
                }
                api.host_input(session, &encrypted).await
            }
            .await;
            if !current() {
                return;
            }
            if let Err(error) = result {
                self.error.set(Some(error));
            }
            self.refresh(false);
        });
    }
    pub fn ask(self, resource: &openwebide_core::host_admin::HostResource) {
        if self.projects.active_project.get_untracked().is_some() {
            return;
        }
        let Some(environment) = self.environment.get_untracked() else {
            return;
        };
        self.chat.draft.update(|draft| {
            if !draft.is_empty() {
                draft.push_str("\n\n");
            }
            draft.push_str(&format!(
                "Inspect this resource on administration host {}:\n{}",
                environment.target,
                serde_json::to_string(resource).unwrap_or_default()
            ));
        });
    }
}
