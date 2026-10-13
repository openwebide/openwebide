//! Host-scoped settings facade; project/session switches do not change deployment ownership.
use crate::{backend::Api, state::auth::AuthState};
use leptos::prelude::*;
use openwebide_core::host_admin::updates::{
    Build, Deployment, UpdateCheck, UpdateJob, UpdateRequest, UpdateResponse, UpdateSettings,
};

#[derive(Clone, Copy)]
pub struct UpdateState {
    pub discovered: RwSignal<Option<Deployment>>,
    pub settings: RwSignal<UpdateSettings>,
    pub running: RwSignal<Option<Build>>,
    pub check: RwSignal<Option<UpdateCheck>>,
    pub job: RwSignal<Option<UpdateJob>>,
    pub busy: RwSignal<bool>,
    pub tracking: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    generation: RwSignal<u64>,
    api: Api,
    auth: AuthState,
}
impl UpdateState {
    pub fn new(api: Api, auth: AuthState) -> Self {
        let state = Self {
            discovered: RwSignal::new(None),
            settings: RwSignal::new(UpdateSettings::default()),
            running: RwSignal::new(None),
            check: RwSignal::new(None),
            job: RwSignal::new(None),
            busy: RwSignal::new(false),
            tracking: RwSignal::new(false),
            error: RwSignal::new(None),
            generation: RwSignal::new(0),
            api,
            auth,
        };
        Effect::new(move |previous: Option<u64>| {
            let epoch = auth.generation.get();
            if previous.is_some_and(|previous| previous != epoch) {
                state.generation.update(|value| *value += 1);
                state.discovered.set(None);
                state.settings.set(UpdateSettings::default());
                state.running.set(None);
                state.check.set(None);
                state.job.set(None);
                state.error.set(None);
                state.busy.set(false);
                state.tracking.set(false);
            }
            epoch
        });
        state
    }
    pub fn request(self, request: UpdateRequest) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        if !matches!(request, UpdateRequest::Status) {
            self.error.set(None);
        }
        if matches!(request, UpdateRequest::Start { .. }) {
            self.tracking.set(true);
        }
        self.generation.update(|value| *value += 1);
        let generation = self.generation.get_untracked();
        let epoch = self.auth.generation.get_untracked();
        let api = self.api.get_value();
        wasm_bindgen_futures::spawn_local(async move {
            let result = api.host_updates(&request).await;
            if self.generation.get_untracked() != generation
                || self.auth.generation.get_untracked() != epoch
            {
                return;
            }
            self.busy.set(false);
            match result {
                Ok(UpdateResponse::Discovered { deployment }) => {
                    self.discovered.set(Some(deployment));
                }
                Ok(UpdateResponse::Settings {
                    settings,
                    running,
                    job,
                }) => {
                    self.settings.set(settings);
                    self.running.set(Some(running));
                    self.tracking
                        .set(job.as_ref().is_some_and(|job| job.phase.active()));
                    self.job.set(job);
                    self.check.set(None);
                }
                Ok(UpdateResponse::Check { check }) => {
                    self.running.set(Some(check.running.clone()));
                    self.check.set(Some(check));
                }
                Ok(UpdateResponse::Status { job, running }) => {
                    if job.is_some() {
                        self.error.set(None);
                    }
                    self.tracking
                        .set(job.as_ref().is_some_and(|job| job.phase.active()));
                    self.running.set(Some(running));
                    self.job.set(job);
                }
                Err(error) => self.error.set(Some(error)),
            }
        });
    }
}
