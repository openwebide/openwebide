//! Host language activation, coalescing and publication for the current editor.
use super::EditorActions;
use crate::{
    backend::Api,
    plugin_actions::ProjectLanguageService,
    project_host::ProjectHost,
    state::{
        languages::LanguageStatus,
        workspace::{EditorSyntaxScope, PreparedEditorSyntax},
    },
};
use leptos::prelude::*;
use openwebide_core::{
    editor::{SyntaxAnalysis, SyntaxStatus},
    plugins::languages::{AnalysisStatus, LanguageOutput, LanguageSelection, RegisteredLanguage},
};
use std::{cell::Cell, rc::Rc, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceScope {
    account: u64,
    project: i64,
    session: Option<i64>,
    host: u64,
    retry: u64,
    provider: RegisteredLanguage,
    registry: openwebide_core::plugins::languages::LanguageRegistry,
    preferences: openwebide_core::plugins::languages::LanguagePreferences,
}

impl EditorActions {
    pub(super) fn language_provider_current(self, scope: &EditorSyntaxScope) -> bool {
        let (Some(state), Some(actions)) = (self.languages, self.language_actions) else {
            return true;
        };
        state
            .selected
            .try_with_untracked(|selected| {
                if !state.settings_loaded.get_untracked() {
                    return selected.is_none();
                }
                actions.provider_current(&scope.key.1, &scope.source, selected.as_ref())
            })
            .unwrap_or(false)
    }
    pub fn is_pair_character(self, ch: char) -> bool {
        let Some(_) = self.current_language_provider() else {
            return false;
        };
        self.language_actions.is_some_and(|actions| {
            actions.registry.with_untracked(|registry| {
                registry
                    .providers
                    .iter()
                    .filter(|candidate| {
                        actions.preferences.with_untracked(|preferences| {
                            matches!(registry.select("", "", Some(&candidate.definition.id), preferences), LanguageSelection::Selected(selected) if selected == *candidate)
                        })
                    })
                    .any(|candidate| {
                        let configuration = &candidate.definition.configuration;
                        configuration
                            .auto_closing_pairs
                            .iter()
                            .any(|pair| pair.open == ch || pair.close == ch)
                            || configuration
                                .surrounding_pairs
                                .iter()
                                .any(|pair| pair.open == ch || pair.close == ch)
                            || configuration
                                .brackets
                                .iter()
                                .any(|pair| pair.open == ch || pair.close == ch)
                    })
            })
        })
    }
    pub(super) fn has_adjacent_bracket(self, source: &str, offset: usize) -> bool {
        if openwebide_core::editor::has_adjacent_bracket(source, offset) {
            return true;
        }
        let (Some(before), Some(after)) = (source.get(..offset), source.get(offset..)) else {
            return false;
        };
        after
            .chars()
            .next()
            .into_iter()
            .chain(before.chars().next_back())
            .any(|ch| self.is_pair_character(ch))
    }
    pub(super) fn has_structural_provider(self) -> bool {
        self.current_language_provider()
            .is_some_and(|provider| provider.definition.features.structure)
    }
    pub(super) fn line_comments_require_structure(self) -> bool {
        self.current_language_provider().is_some_and(|provider| {
            provider.definition.features.structure
                && !provider.definition.configuration.uniform_line_comments
        })
    }
    pub fn language_generation(self) -> u64 {
        self.languages
            .map_or(0, |state| state.generation.get_untracked())
    }

    pub(super) fn configured_analysis(self, source: Arc<String>) -> Option<Arc<SyntaxAnalysis>> {
        let provider = self.current_language_provider()?;
        SyntaxAnalysis::from_language_configuration(
            source,
            &provider.definition.id,
            std::slice::from_ref(&provider.definition),
        )
        .ok()
        .map(Arc::new)
    }

    pub(super) fn current_language_provider(self) -> Option<RegisteredLanguage> {
        let state = self.languages?;
        if !state.settings_loaded.try_get_untracked()? {
            return None;
        }
        let provider = state.selected.try_get_untracked()??;
        let key = self.key()?;
        self.workspace.content.try_with_untracked(|source| {
            self.language_actions?
                .provider_current(&key.1, source, Some(&provider))
                .then_some(provider)
        })?
    }

    pub(super) fn install_language_services(self) {
        let (Some(state), Some(actions), Some(api), Some(host)) = (
            self.languages,
            self.language_actions,
            use_context::<Api>(),
            use_context::<ProjectHost>(),
        ) else {
            return;
        };
        let activation = StoredValue::new(None::<ServiceScope>);
        let service = StoredValue::new_local(None::<(ServiceScope, ProjectLanguageService)>);
        let active = StoredValue::new(None::<EditorSyntaxScope>);
        let pending = StoredValue::new(None::<(u64, EditorSyntaxScope, ServiceScope)>);
        let ticket = StoredValue::new(0_u64);
        let running = Rc::new(Cell::new(false));
        let document = StoredValue::new(None::<((i64, String), u64, String)>);
        let document_sequence = StoredValue::new(0_u64);
        let mut nonce = [0_u8; 16];
        let Ok(()) = web_sys::window()
            .ok_or(())
            .and_then(|window| window.crypto().map_err(|_| ()))
            .and_then(|crypto| {
                crypto
                    .get_random_values_with_u8_array(&mut nonce)
                    .map(|_| ())
                    .map_err(|_| ())
            })
        else {
            state.status.set(LanguageStatus::Unavailable(
                "Language session identity is unavailable".into(),
            ));
            return;
        };
        let nonce = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let generation = StoredValue::new(None::<u64>);
        on_cleanup(move || {
            if !self.workspace.editor_worker_active.is_disposed()
                && generation
                    .try_get_value()
                    .flatten()
                    .is_some_and(|generation| {
                        state.generation.try_get_untracked() == Some(generation)
                    })
            {
                // Cancel this editor's captured actions without invalidating a
                // newer editor that already owns the shared language state.
                state
                    .generation
                    .update(|value| *value = value.wrapping_add(1));
                self.workspace.editor_worker_active.set(false);
                self.workspace.editor_preparation.set(None);
            }
        });
        Effect::new(move || {
            self.workspace.content.track();
            self.workspace.editor_source_revision.track();
            self.workspace.open_file.track();
            self.workspace.active_project.track();
            self.workspace.active_session.track();
            self.workspace.pending_epoch.track();
            self.workspace.editor_read_revision.track();
            self.rules();
            if let Some(auth) = self.auth {
                auth.generation.track();
            }
            host.track_revision();
            let retry = state.retry.get();
            let loaded = actions.ready.get();
            let settings_error = state.settings_error.get();
            let preferences = actions.preferences.get();
            let registry = actions.registry.get();
            let key = self.key();
            let selection = key.as_ref().map(|key| {
                self.workspace
                    .content
                    .with_untracked(|source| registry.select(&key.1, source, None, &preferences))
            });
            let (selected, status) = if key.is_none() {
                (None, LanguageStatus::Plain)
            } else if !loaded {
                (
                    None,
                    LanguageStatus::Unavailable(
                        settings_error.unwrap_or_else(|| "Loading language preferences".into()),
                    ),
                )
            } else {
                match selection {
                    Some(LanguageSelection::Selected(provider)) => {
                        (Some(provider.clone()), LanguageStatus::Preparing)
                    }
                    Some(LanguageSelection::Ambiguous(providers)) => (
                        None,
                        LanguageStatus::Conflicting(providers.into_iter().cloned().collect()),
                    ),
                    Some(LanguageSelection::Unavailable(provider)) => (
                        None,
                        LanguageStatus::Unavailable(format!(
                            "Preferred language provider is unavailable: {provider}"
                        )),
                    ),
                    _ => (None, LanguageStatus::Plain),
                }
            };
            let identity = selected
                .as_ref()
                .zip(key.as_ref())
                .and_then(|(provider, key)| {
                    Some(ServiceScope {
                        account: self.account_generation(),
                        project: key.0,
                        session: self.workspace.active_session.get_untracked(),
                        host: host.revision()?,
                        retry,
                        provider: provider.clone(),
                        registry: registry.clone(),
                        preferences: preferences.clone(),
                    })
                });
            if activation.with_value(|previous| previous.as_ref() != identity.as_ref()) {
                activation.set_value(identity.clone());
                service.set_value(None);
                document.set_value(None);
                state
                    .generation
                    .update(|value| *value = value.wrapping_add(1));
                generation.set_value(Some(state.generation.get_untracked()));
                state.selected.set(selected.clone());
                self.workspace.editor_preparation.set(None);
                self.workspace
                    .editor_preparation_revision
                    .update(|value| *value = value.wrapping_add(1));
            }
            state.status.set(status);
            ticket.update_value(|value| *value = value.wrapping_add(1));
            let next = ticket.get_value();
            pending.set_value(None);
            self.workspace.editor_worker_active.set(identity.is_some());
            let Some(identity) = identity else { return };
            let Some(scope) = self.syntax_scope() else {
                return;
            };
            if !identity.provider.definition.features.syntax
                && !identity.provider.definition.features.folding
                && !identity.provider.definition.features.structure
            {
                if let Some(analysis) = self.configured_analysis(scope.source.clone()) {
                    self.workspace
                        .editor_preparation
                        .set(Some(PreparedEditorSyntax {
                            scope,
                            status: SyntaxStatus::Ready { incremental: false },
                            analysis: Some(analysis),
                        }));
                    self.workspace
                        .editor_preparation_revision
                        .update(|value| *value = value.wrapping_add(1));
                    state.status.set(LanguageStatus::Ready);
                }
                return;
            }
            pending.set_value(Some((next, scope, identity)));
            if running.replace(true) {
                return;
            }
            let running = running.clone();
            let nonce = nonce.clone();
            leptos::task::spawn_local(async move {
                while !pending.is_disposed() {
                    let Some((request_ticket, scope, identity)) =
                        pending.try_update_value(Option::take).flatten()
                    else {
                        break;
                    };
                    let current = || {
                        !pending.is_disposed()
                            && ticket.get_value() == request_ticket
                            && self.syntax_scope_current(&scope)
                    };
                    if !current() {
                        continue;
                    }
                    if scope.source.len() > openwebide_core::plugins::languages::MAX_SOURCE_BYTES {
                        state.status.set(LanguageStatus::TooLarge);
                        self.workspace
                            .editor_preparation
                            .set(Some(PreparedEditorSyntax {
                                scope,
                                status: SyntaxStatus::TooLarge,
                                analysis: None,
                            }));
                        self.workspace
                            .editor_preparation_revision
                            .update(|value| *value = value.wrapping_add(1));
                        continue;
                    }
                    active.set_value(Some(scope.clone()));
                    let cached = service
                        .get_value()
                        .filter(|(previous, _)| *previous == identity)
                        .map(|(_, client)| client);
                    let client = if let Some(client) = cached {
                        Ok(client)
                    } else {
                        let expected = identity.clone();
                        ProjectLanguageService::new(
                            api,
                            host,
                            identity.project,
                            identity.provider.clone(),
                            identity.preferences.clone(),
                            move || {
                                activation
                                    .try_with_value(|scope| scope.as_ref() == Some(&expected))
                                    .unwrap_or(false)
                                    && active
                                        .try_with_value(|scope| {
                                            scope.as_ref().is_some_and(|scope| {
                                                self.syntax_scope_current(scope)
                                            })
                                        })
                                        .unwrap_or(false)
                                    && actions.providers_current(
                                        &expected.registry,
                                        &expected.preferences,
                                    )
                                    && host.revision() == Some(expected.host)
                            },
                        )
                        .await
                    };
                    if !current() {
                        continue;
                    }
                    let result = match client {
                        Err(error) => Err(error),
                        Ok(client) => {
                            service.set_value(Some((identity.clone(), client.clone())));
                            let previous = document.get_value();
                            let name = if let Some((key, read, name)) = &previous
                                && key == &scope.key
                                && *read == scope.read_revision
                            {
                                name.clone()
                            } else {
                                if let Some((_, _, name)) = previous {
                                    let _ = client.close(&name).await;
                                }
                                document_sequence
                                    .update_value(|value| *value = value.wrapping_add(1));
                                let name = format!("{nonce}:{}", document_sequence.get_value());
                                document.set_value(Some((
                                    scope.key.clone(),
                                    scope.read_revision,
                                    name.clone(),
                                )));
                                name
                            };
                            if !current() {
                                continue;
                            }
                            client
                                .analyze(
                                    openwebide_agent::plugins::languages::LanguageDocumentRequest {
                                        document: name,
                                        path: scope.key.1.clone(),
                                        revision: scope.source_revision,
                                        source: scope.source.clone(),
                                        tab_width: scope.tab_width,
                                    },
                                )
                                .await
                                .and_then(|output| match output {
                                    LanguageOutput::Analyzed(response) => match response.status {
                                        AnalysisStatus::Ready { incremental } => {
                                            let data = response
                                                .analysis
                                                .ok_or("Language result has no analysis")?;
                                            let previous = self
                                                .workspace
                                                .editor_preparation
                                                .with_untracked(|prepared| {
                                                    let prepared = prepared.as_ref()?;
                                                    let mut owner = prepared.scope.clone();
                                                    owner.source = scope.source.clone();
                                                    owner.source_revision = scope.source_revision;
                                                    (owner == scope)
                                                        .then(|| prepared.analysis.clone())
                                                        .flatten()
                                                });
                                            SyntaxAnalysis::from_language_data_reusing(
                                                scope.source.clone(),
                                                &identity.provider.definition.id,
                                                data,
                                                client.definitions(),
                                                previous.as_deref(),
                                            )
                                            .map(
                                                |analysis| {
                                                    (
                                                        SyntaxStatus::Ready { incremental },
                                                        Some(Arc::new(analysis)),
                                                    )
                                                },
                                            )
                                        }
                                        AnalysisStatus::TooLarge => {
                                            Ok((SyntaxStatus::TooLarge, None))
                                        }
                                        _ => {
                                            Err("Language provider could not analyze this document"
                                                .into())
                                        }
                                    },
                                    LanguageOutput::Closed { .. } => {
                                        Err("Unexpected language close response".into())
                                    }
                                })
                        }
                    };
                    if !current() {
                        continue;
                    }
                    match result {
                        Ok((status, analysis)) => {
                            state.status.set(if status == SyntaxStatus::TooLarge {
                                LanguageStatus::TooLarge
                            } else {
                                LanguageStatus::Ready
                            });
                            self.workspace
                                .editor_preparation
                                .set(Some(PreparedEditorSyntax {
                                    scope,
                                    status,
                                    analysis,
                                }));
                        }
                        Err(error) => {
                            state.status.set(LanguageStatus::Unavailable(error));
                            self.workspace.editor_worker_active.set(false);
                            self.workspace.editor_preparation.set(None);
                        }
                    }
                    self.workspace
                        .editor_preparation_revision
                        .update(|value| *value = value.wrapping_add(1));
                }
                running.set(false);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::EditorCommand;
    use super::*;
    use crate::state::{
        languages::LanguagesState,
        workspace::{EditorText, WorkspaceState},
    };
    use openwebide_core::editor::{FoldRange, Indentation, Selection};

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn configuration_only_languages_apply_declared_rules_without_host_execution_in_both_modes()
     {
        use crate::{
            backend::Backend,
            project_languages::ProjectLanguageActions,
            state::{
                auth::AuthState,
                plugins::PluginsState,
                projects::ProjectsState,
                settings::{SettingsState, Theme},
            },
            testing::fake_backend::{Call, FakeBackend},
        };
        use openwebide_core::{Project, User, UserId, UserRole, WorkspaceMode};
        crate::testing::initialize_runtime();
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let owner = Owner::new();
            let (workspace, state, plugins, fake, actions) = owner.with(|| {
                let fake = Rc::new(FakeBackend::default());
                let api = StoredValue::new_local(fake.clone() as Rc<dyn Backend>);
                let auth = AuthState::new();
                auth.set_user(User { id: UserId::new(1), username: "owner".into(), role: UserRole::User, created_at: 0 });
                let projects = ProjectsState::new();
                projects.projects.set(vec![Project { id: 1, name: "Metadata".into(), mode, path: Some("project".into()), user_id: None, created_at: 0 }]);
                projects.active_project.set(Some(1));
                let settings = SettingsState::new(Theme::Dark, "http://127.0.0.1:1".into());
                let host = ProjectHost::new(api, projects, settings, auth);
                let plugins = PluginsState::default();
                let binding: openwebide_core::plugins::ProjectPlugin = serde_json::from_value(serde_json::json!({
                    "id":1,"revision":1,"enabled":true,"prepared":{
                        "source":{"repository":"https://example.test/metadata.git","commit":"a".repeat(40),"path":"."},
                        "digest":"b".repeat(64),"host_id":"fixture-host","manifest":{
                            "schemaVersion":1,"publisher":"example","name":"metadata","version":"0.1.0",
                            "displayName":"Metadata","description":"Declarative language fixture","license":"MIT",
                            "compatibility":{"pluginApi":5},"contributions":{"skills":[],"languages":[{
                                "id":"metadata-notes","name":"Metadata Notes","extensions":["metadata"],
                                "configuration":{"lineComment":"#","brackets":[{"open":"«","close":"»"}],
                                    "autoClosingPairs":[{"open":"«","close":"»"}],"surroundingPairs":[{"open":"«","close":"»"}]}
                            }]}
                        }
                    }
                })).unwrap();
                plugins.project_plugins.set(vec![binding]);
                let state = LanguagesState::default();
                provide_context(settings);
                let language_actions = ProjectLanguageActions::new(api, state, auth, projects, plugins);
                let workspace = WorkspaceState::with_active_project(projects.active_project);
                workspace.open_file.set(Some("example.metadata".into()));
                provide_context(api); provide_context(auth); provide_context(host); provide_context(settings);
                provide_context(state); provide_context(language_actions);
                let actions = EditorActions::new(workspace);
                actions.install_syntax_worker();
                (workspace, state, plugins, fake, actions)
            });
            for _ in 0..100 {
                if state.status.get_untracked() == LanguageStatus::Ready {
                    break;
                }
                crate::util::sleep_ms(10).await;
            }
            assert_eq!(state.status.get_untracked(), LanguageStatus::Ready);
            assert!(actions.command_supported(EditorCommand::LineComment));
            assert!(!actions.command_supported(EditorCommand::Reindent));
            assert!(actions.is_pair_character('«'));
            assert!(!actions.is_pair_character('('));
            actions
                .command(
                    EditorCommand::TypeCharacter('«'),
                    Selection::caret(0),
                    Indentation::default(),
                )
                .unwrap();
            assert_eq!(workspace.content.get_untracked().as_str(), "«»");
            actions
                .command(
                    EditorCommand::LineComment,
                    Selection::caret(0),
                    Indentation::default(),
                )
                .unwrap();
            assert_eq!(workspace.content.get_untracked().as_str(), "# «»");
            let scope = actions.syntax_scope().unwrap();
            plugins
                .project_plugins
                .update(|plugins| plugins[0].enabled = false);
            // Revoke synchronously, before the activation effect has cleared
            // its selected provider and advanced the generation.
            assert!(!actions.syntax_scope_current(&scope));
            assert!(!actions.is_pair_character('«'));
            assert!(!actions.command_supported(EditorCommand::LineComment));
            assert_eq!(workspace.content.get_untracked().as_str(), "# «»");
            assert!(!fake.calls.borrow().iter().any(|call| matches!(
                call,
                Call::Request {
                    method: "start_plugin_invocation"
                        | "continue_plugin_invocation"
                        | "plugin_context_grants"
                }
            )));
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn terminal_plain_results_reveal_old_folds_without_losing_edits_or_undo() {
        crate::testing::initialize_runtime();
        for unavailable in [false, true] {
            Owner::new().with(|| {
                let workspace = WorkspaceState::new();
                workspace.active_project.set(Some(1));
                workspace.open_file.set(Some("fixture.notes".into()));
                workspace
                    .content
                    .set(EditorText::from("note {\n task\n}\n"));
                let state = LanguagesState::default();
                provide_context(state);
                let actions = EditorActions::new(workspace);
                actions
                    .command(
                        EditorCommand::TypeCharacter('x'),
                        Selection::caret(0),
                        Indentation::default(),
                    )
                    .unwrap();
                workspace.editor_documents.update(|documents| {
                    let document = actions.document(documents, (1, "fixture.notes".into()));
                    document.set_fold_ranges(vec![FoldRange {
                        start_line: 0,
                        end_line: 2,
                    }]);
                    document.fold_state_mut().collapse_all();
                    assert_eq!(document.fold_state().collapsed_ranges().count(), 1);
                });
                state.status.set(if unavailable {
                    LanguageStatus::Unavailable("host disconnected".into())
                } else {
                    LanguageStatus::Plain
                });
                workspace.editor_worker_active.set(false);
                workspace.editor_preparation.set(None);
                actions.refresh_fold_ranges(|| true).unwrap();
                workspace.editor_documents.with_untracked(|documents| {
                    let document = &documents[&(1, "fixture.notes".into())];
                    assert!(document.fold_state().ranges().is_empty());
                    assert_eq!(document.fold_state().collapsed_ranges().count(), 0);
                    assert_eq!(document.text(), "xnote {\n task\n}\n");
                });
                actions
                    .command(
                        EditorCommand::Undo,
                        Selection::caret(1),
                        Indentation::default(),
                    )
                    .unwrap();
                assert_eq!(
                    workspace.content.get_untracked().as_str(),
                    "note {\n task\n}\n"
                );
            });
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn provider_and_session_changes_reject_analysis_without_replacing_native_input_ownership() {
        crate::testing::initialize_runtime();
        Owner::new().with(|| {
            let workspace = WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("fixture.notes".into()));
            workspace.content.set(EditorText::from("文😀\r\n"));
            let state = LanguagesState::default();
            provide_context(state);
            let actions = EditorActions::new(workspace);
            let input = actions.presentation_scope().unwrap();
            let scope = actions.syntax_scope().unwrap();
            assert!(actions.syntax_scope_current(&scope));
            state.generation.update(|value| *value += 1);
            assert!(!actions.syntax_scope_current(&scope));
            assert_eq!(actions.presentation_scope(), Some(input.clone()));
            let scope = actions.syntax_scope().unwrap();
            workspace.active_session.set(Some(22));
            assert!(!actions.syntax_scope_current(&scope));
            assert_eq!(actions.presentation_scope(), Some(input));
        });
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn unmounted_editor_actions_cannot_revalidate_or_replace_a_live_workspaces_syntax_scope() {
        crate::testing::initialize_runtime();
        Owner::new().with(|| {
            let workspace = WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("fixture.notes".into()));
            workspace.content.set(EditorText::from("note 文😀"));
            let editor = Owner::new();
            let (actions, scope) = editor.with(|| {
                let actions = EditorActions::new(workspace);
                (actions, actions.syntax_scope().unwrap())
            });
            editor.cleanup();
            assert!(!actions.syntax_scope_current(&scope));
            assert!(actions.syntax_scope().is_none());
            assert_eq!(workspace.editor_syntax_scope.get_untracked(), Some(scope));
            assert_eq!(workspace.content.get_untracked().as_str(), "note 文😀");
        });
    }
}
