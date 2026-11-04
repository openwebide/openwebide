//! Database preferences and provider discovery above the shared host service.
use crate::{
    backend::Api,
    state::{
        auth::AuthState, languages::LanguagesState, plugins::PluginsState, projects::ProjectsState,
    },
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::plugins::languages::{
    LanguagePreferences, LanguageRegistry, LanguageSettings,
};

const SETTINGS_KEY: &str = "language_settings";
mod previews;
pub(crate) use previews::{preview_diff, preview_patch_syntax};

#[derive(Clone, Copy)]
pub struct ProjectLanguageActions {
    pub ready: Memo<bool>,
    pub registry: Memo<LanguageRegistry>,
    pub preferences: Memo<LanguagePreferences>,
    pub save: Callback<LanguageSettings>,
    pub reload: Callback<()>,
}

impl ProjectLanguageActions {
    pub(crate) fn providers_current(
        self,
        registry: &LanguageRegistry,
        preferences: &LanguagePreferences,
    ) -> bool {
        self.registry
            .try_with_untracked(|current| current == registry)
            .unwrap_or(false)
            && self
                .preferences
                .try_with_untracked(|current| current == preferences)
                .unwrap_or(false)
    }
    pub(crate) fn provider_current(
        self,
        path: &str,
        source: &str,
        expected: Option<&openwebide_core::plugins::languages::RegisteredLanguage>,
    ) -> bool {
        use openwebide_core::plugins::languages::LanguageSelection;
        if !self.ready.try_get_untracked().unwrap_or(false) {
            return expected.is_none();
        }
        self.registry
            .try_with_untracked(|registry| {
                self.preferences
                    .try_with_untracked(|preferences| {
                        match registry.select(path, source, None, preferences) {
                            LanguageSelection::Selected(provider) => Some(provider) == expected,
                            _ => expected.is_none(),
                        }
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }
    pub fn new(
        api: Api,
        state: LanguagesState,
        auth: AuthState,
        projects: ProjectsState,
        plugins: PluginsState,
    ) -> Self {
        let settings = expect_context::<crate::state::settings::SettingsState>();
        let loaded_account = RwSignal::new(None::<u64>);
        let ready = Memo::new(move |_| {
            state.settings_loaded.get()
                && loaded_account.get() == Some(auth.generation.get())
                && auth.user.with(Option::is_some)
        });
        let registry = Memo::new(move |_| {
            if !ready.get() {
                return LanguageRegistry::default();
            }
            projects.active_project.track();
            plugins
                .project_plugins
                .with(|bindings| LanguageRegistry::from_bindings(bindings))
        });
        let preferences = Memo::new(move |_| {
            state.settings.with(|settings| {
                projects.active_project.get().map_or_else(
                    || settings.defaults.clone(),
                    |project| settings.for_project(project),
                )
            })
        });
        let reload_revision = RwSignal::new(0_u64);
        let load_ticket = StoredValue::new(0_u64);
        let pending = StoredValue::new(None::<LanguageSettings>);
        let confirmed = StoredValue::new(LanguageSettings::default());
        let saving_account = StoredValue::new(None::<u64>);
        Effect::new(move |_| {
            let account = auth.generation.get();
            reload_revision.track();
            let logged_in = auth.user.with_untracked(Option::is_some);
            load_ticket.update_value(|value| *value = value.wrapping_add(1));
            let ticket = load_ticket.get_value();
            pending.set_value(None);
            confirmed.set_value(LanguageSettings::default());
            state.settings.set(LanguageSettings::default());
            state.settings_loaded.set(false);
            loaded_account.set(None);
            state.settings_error.set(None);
            state.settings_saving.set(false);
            if !logged_in {
                return;
            }
            spawn_local(async move {
                let result = settings.load_values(api, account).await.and_then(|values| {
                    let settings = values.get(SETTINGS_KEY).map_or_else(
                        || Ok(LanguageSettings::default()),
                        |value| {
                            serde_json::from_str::<LanguageSettings>(value)
                                .map_err(|_| "Saved language preferences are invalid".to_string())
                        },
                    )?;
                    settings.validate()?;
                    Ok(settings)
                });
                if auth.generation.try_get_untracked() != Some(account)
                    || load_ticket.try_get_value() != Some(ticket)
                {
                    return;
                }
                match result {
                    Ok(settings) => {
                        confirmed.set_value(settings.clone());
                        state.settings.set(settings);
                        loaded_account.set(Some(account));
                        state.settings_loaded.set(true);
                    }
                    Err(error) => state.settings_error.set(Some(error)),
                }
            });
        });
        let save = Callback::new(move |settings: LanguageSettings| {
            if !ready.get_untracked() {
                return;
            }
            if let Err(error) = settings.validate() {
                state.settings_error.set(Some(error));
                return;
            }
            let account = auth.generation.get_untracked();
            state.settings.set(settings.clone());
            state.settings_error.set(None);
            pending.set_value(Some(settings));
            if saving_account.get_value() == Some(account) {
                return;
            }
            saving_account.set_value(Some(account));
            state.settings_saving.set(true);
            spawn_local(async move {
                while auth.generation.try_get_untracked() == Some(account) {
                    let Some(settings) = pending.try_update_value(Option::take).flatten() else {
                        break;
                    };
                    let value =
                        serde_json::to_string(&settings).expect("language settings serialize");
                    let result = api
                        .with_value(Clone::clone)
                        .set_setting(SETTINGS_KEY, &value)
                        .await;
                    if auth.generation.try_get_untracked() != Some(account) {
                        return;
                    }
                    match result {
                        Ok(()) => confirmed.set_value(settings),
                        Err(error) => {
                            if pending.get_value().is_none() {
                                state.settings.set(confirmed.get_value());
                            }
                            state.settings_error.set(Some(format!(
                                "Could not save language preferences: {error}"
                            )));
                        }
                    }
                }
                if saving_account.try_get_value() == Some(Some(account)) {
                    saving_account.set_value(None);
                    state.settings_saving.set(false);
                }
            });
        });
        let reload = Callback::new(move |()| {
            if !state.settings_saving.get_untracked() {
                settings.refresh_values();
                reload_revision.update(|revision| *revision = revision.wrapping_add(1));
            }
        });
        Self {
            ready,
            registry,
            preferences,
            save,
            reload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{backend::Backend, testing::fake_backend::FakeBackend};
    use openwebide_core::{
        Project, User, UserId, UserRole, WorkspaceMode, plugins::languages::ProviderPreference,
    };
    use std::rc::Rc;

    async fn until(mut condition: impl FnMut() -> bool) {
        for _ in 0..100 {
            if condition() {
                return;
            }
            crate::util::sleep_ms(10).await;
        }
        assert!(condition(), "language preferences did not settle");
    }
    fn user(auth: AuthState, id: i64) {
        auth.set_user(User {
            id: UserId::new(id),
            username: format!("user-{id}"),
            role: UserRole::User,
            created_at: 0,
        });
    }
    fn fixture(
        mode: WorkspaceMode,
    ) -> (
        Owner,
        Rc<FakeBackend>,
        AuthState,
        LanguagesState,
        ProjectLanguageActions,
    ) {
        crate::testing::initialize_runtime();
        let owner = Owner::new();
        let fake = Rc::new(FakeBackend::default());
        let (auth, state, actions) = owner.with(|| {
            let api = StoredValue::new_local(fake.clone() as Rc<dyn Backend>);
            let auth = AuthState::new();
            user(auth, 1);
            let projects = ProjectsState::new();
            projects.projects.set(vec![Project {
                id: 1,
                name: "Project".into(),
                path: Some("project".into()),
                mode,
                user_id: None,
                created_at: 0,
            }]);
            projects.active_project.set(Some(1));
            let state = LanguagesState::default();
            provide_context(crate::state::settings::SettingsState::new(
                crate::state::settings::Theme::System,
                String::new(),
            ));
            let actions =
                ProjectLanguageActions::new(api, state, auth, projects, PluginsState::default());
            (auth, state, actions)
        });
        (owner, fake, auth, state, actions)
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn language_choices_are_database_backed_and_project_overrides_survive_reload_in_both_modes()
     {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, fake, _, state, actions) = fixture(mode);
            until(|| state.settings_loaded.get_untracked()).await;
            let mut settings = LanguageSettings::default();
            settings
                .defaults
                .providers
                .insert("notes".into(), ProviderPreference::Disabled);
            settings
                .projects
                .entry(1)
                .or_default()
                .providers
                .insert("notes".into(), ProviderPreference::Automatic);
            actions.save.run(settings.clone());
            until(|| !state.settings_saving.get_untracked()).await;
            assert_eq!(
                serde_json::from_str::<LanguageSettings>(&fake.settings.borrow()[SETTINGS_KEY])
                    .unwrap(),
                settings
            );
            actions.reload.run(());
            crate::util::yield_task().await;
            until(|| state.settings_loaded.get_untracked()).await;
            assert_eq!(state.settings.get_untracked(), settings);
            assert_eq!(
                actions.preferences.get_untracked().providers["notes"],
                ProviderPreference::Automatic
            );
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn queued_language_choices_save_in_order_and_failure_restores_the_confirmed_choice() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, fake, _, state, actions) = fixture(mode);
            until(|| state.settings_loaded.get_untracked()).await;
            let (finish, pending) = futures::channel::oneshot::channel();
            fake.language_save_results.borrow_mut().push_back(pending);
            let mut first = LanguageSettings::default();
            first
                .defaults
                .providers
                .insert("notes".into(), ProviderPreference::Disabled);
            actions.save.run(first.clone());
            crate::util::yield_task().await;
            let mut second = first.clone();
            second
                .projects
                .entry(1)
                .or_default()
                .providers
                .insert("notes".into(), ProviderPreference::Automatic);
            actions.save.run(second.clone());
            finish.send(Ok(())).unwrap();
            until(|| !state.settings_saving.get_untracked()).await;
            assert_eq!(
                serde_json::from_str::<LanguageSettings>(&fake.settings.borrow()[SETTINGS_KEY])
                    .unwrap(),
                second
            );
            let (finish, pending) = futures::channel::oneshot::channel();
            fake.language_save_results.borrow_mut().push_back(pending);
            actions.save.run(first);
            finish.send(Err("offline".into())).unwrap();
            until(|| !state.settings_saving.get_untracked()).await;
            assert_eq!(state.settings.get_untracked(), second);
            assert!(
                state
                    .settings_error
                    .get_untracked()
                    .unwrap()
                    .contains("offline")
            );
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn account_changes_revoke_language_choices_before_load_effects_run() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, fake, auth, state, actions) = fixture(mode);
            until(|| actions.ready.get_untracked()).await;
            user(auth, 2);
            assert!(!actions.ready.get_untracked());
            let mut stale = LanguageSettings::default();
            stale
                .defaults
                .providers
                .insert("notes".into(), ProviderPreference::Disabled);
            actions.save.run(stale);
            assert!(!fake.settings.borrow().contains_key(SETTINGS_KEY));
            until(|| actions.ready.get_untracked()).await;
            assert_eq!(state.settings.get_untracked(), LanguageSettings::default());
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn delayed_preference_loads_and_saves_cannot_replace_another_accounts_choices() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, fake, auth, state, actions) = fixture(mode);
            until(|| state.settings_loaded.get_untracked()).await;
            let (finish, pending) = futures::channel::oneshot::channel();
            fake.settings_load_results.borrow_mut().push_back(pending);
            actions.reload.run(());
            crate::util::yield_task().await;
            user(auth, 2);
            crate::util::yield_task().await;
            until(|| state.settings_loaded.get_untracked()).await;
            let mut old = LanguageSettings::default();
            old.defaults
                .providers
                .insert("notes".into(), ProviderPreference::Disabled);
            finish
                .send(Ok([(
                    SETTINGS_KEY.into(),
                    serde_json::to_string(&old).unwrap(),
                )]
                .into()))
                .unwrap();
            crate::util::yield_task().await;
            assert_eq!(state.settings.get_untracked(), LanguageSettings::default());
            let (finish, pending) = futures::channel::oneshot::channel();
            fake.language_save_results.borrow_mut().push_back(pending);
            actions.save.run(old);
            crate::util::yield_task().await;
            user(auth, 3);
            crate::util::yield_task().await;
            until(|| state.settings_loaded.get_untracked()).await;
            finish.send(Err("old save failed".into())).unwrap();
            crate::util::yield_task().await;
            assert_eq!(state.settings.get_untracked(), LanguageSettings::default());
            assert!(state.settings_error.get_untracked().is_none());
        }
    }
}
