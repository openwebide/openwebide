//! Read-only preview consumers of the same host document service as the editor.
use super::ProjectLanguageActions;
use crate::{
    backend::Api,
    plugin_actions::ProjectLanguageService,
    project_host::ProjectHost,
    state::{
        auth::AuthState, languages::LanguagesState, projects::ProjectsState,
        workspace::WorkspaceState,
    },
};
use leptos::prelude::*;
use openwebide_agent::plugins::languages::LanguageDocumentRequest;
use openwebide_core::{
    editor::SyntaxAnalysis,
    plugins::languages::{AnalysisStatus, LanguageOutput, LanguageSelection, RegisteredLanguage},
};
use std::{cell::Cell, rc::Rc, sync::Arc};

pub(crate) fn preview_diff(
    diff: Memo<openwebide_core::FileDiff>,
) -> Memo<openwebide_core::diff::DiffPaint> {
    let path = Memo::new(move |_| diff.with(|diff| diff.path.clone()));
    let old = Memo::new(move |_| Arc::new(diff.with(|diff| diff.old.clone().unwrap_or_default())));
    let new = Memo::new(move |_| Arc::new(diff.with(|diff| diff.new.clone())));
    let old_syntax = preview_syntax(path.into(), old.into());
    let new_syntax = preview_syntax(path.into(), new.into());
    Memo::new(move |_| {
        diff.with(|diff| {
            old_syntax.with(|old| {
                new_syntax.with(|new| {
                    openwebide_core::diff::DiffPaint::from_tokens(
                        diff,
                        old.as_ref()
                            .filter(|analysis| {
                                analysis.matches_source(diff.old.as_deref().unwrap_or_default())
                            })
                            .and_then(|analysis| analysis.highlights())
                            .map(AsRef::as_ref),
                        new.as_ref()
                            .filter(|analysis| analysis.matches_source(&diff.new))
                            .and_then(|analysis| analysis.highlights())
                            .map(AsRef::as_ref),
                    )
                })
            })
        })
    })
}

/// A preview never selects a local lexer. Source/provider/host changes revoke
/// earlier colors; unavailable services leave the ordinary diff text visible.
type ProviderScope = (
    RegisteredLanguage,
    u64,
    Option<i64>,
    u64,
    openwebide_core::plugins::languages::LanguageRegistry,
    openwebide_core::plugins::languages::LanguagePreferences,
);
type PreviewSource = (String, Arc<String>);
type PreviewRequest = (u64, PreviewSource, ProviderScope, String, i64);

pub(crate) fn preview_syntax(
    path: Signal<String>,
    source: Signal<Arc<String>>,
) -> Signal<Option<Arc<SyntaxAnalysis>>> {
    syntax(path, source, true)
}

/// A patch excerpt may start far below the first line of a file. Do not treat
/// its first displayed hunk as a file signature when choosing a provider.
pub(crate) fn preview_patch_syntax(
    path: Signal<String>,
    source: Signal<Arc<String>>,
) -> Signal<Option<Arc<SyntaxAnalysis>>> {
    syntax(path, source, false)
}

fn syntax(
    path: Signal<String>,
    source: Signal<Arc<String>>,
    complete: bool,
) -> Signal<Option<Arc<SyntaxAnalysis>>> {
    let (
        Some(api),
        Some(host),
        Some(auth),
        Some(projects),
        Some(workspace),
        Some(state),
        Some(actions),
    ) = (
        use_context::<Api>(),
        use_context::<ProjectHost>(),
        use_context::<AuthState>(),
        use_context::<ProjectsState>(),
        use_context::<WorkspaceState>(),
        use_context::<LanguagesState>(),
        use_context::<ProjectLanguageActions>(),
    )
    else {
        return Signal::derive(|| None);
    };
    let origin = (
        auth.generation.get_untracked(),
        projects.active_project.get_untracked(),
    );
    let snapshot = Memo::new(move |_| (path.get(), source.get()));
    let result = RwSignal::new(None::<(ProviderScope, PreviewSource, Arc<SyntaxAnalysis>)>);
    let ticket = StoredValue::new(0_u64);
    let active = StoredValue::new(None::<(String, Arc<String>)>);
    let activation = StoredValue::new(None::<ProviderScope>);
    let activation_generation = StoredValue::new(0_u64);
    let cached = StoredValue::new_local(None::<ProjectLanguageService>);
    let pending = StoredValue::new(None::<PreviewRequest>);
    let running = Rc::new(Cell::new(false));
    let mut nonce = [0_u8; 16];
    let identity = web_sys::window()
        .and_then(|window| window.crypto().ok())
        .and_then(|crypto| crypto.get_random_values_with_u8_array(&mut nonce).ok())
        .map(|_| {
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        });
    Effect::new(move |_| {
        auth.generation.track();
        projects.active_project.track();
        let session = workspace.active_session.get();
        host.track_revision();
        let source = snapshot.get();
        let preferences = actions.preferences.get();
        let registry = actions.registry.get();
        let loaded = actions.ready.get();
        let retry = state.retry.get();
        ticket.update_value(|value| *value = value.wrapping_add(1));
        let next = ticket.get_value();
        result.set(None);
        pending.set_value(None);
        let retire = || {
            activation.set_value(None);
            cached.set_value(None);
        };
        let Some(document) = identity.clone() else {
            retire();
            return;
        };
        let Some(project) = origin.1 else {
            retire();
            return;
        };
        if !loaded
            || origin
                != (
                    auth.generation.get_untracked(),
                    projects.active_project.get_untracked(),
                )
            || source.1.is_empty()
            || source.1.len() > openwebide_core::plugins::languages::MAX_SOURCE_BYTES
        {
            retire();
            return;
        }
        let LanguageSelection::Selected(provider) = registry.select(
            &source.0,
            if complete { &source.1 } else { "" },
            None,
            &preferences,
        ) else {
            retire();
            return;
        };
        if !provider.definition.features.syntax {
            retire();
            return;
        }
        let Some(revision) = host.revision() else {
            retire();
            return;
        };
        let scope = (
            provider.clone(),
            revision,
            session,
            retry,
            registry.clone(),
            preferences.clone(),
        );
        if activation.with_value(|previous| previous.as_ref() != Some(&scope)) {
            activation_generation.update_value(|value| *value = value.wrapping_add(1));
            activation.set_value(Some(scope.clone()));
            cached.set_value(None);
        }
        // A replacement service restarts its request sequence. Its document
        // identity must also change so a warm host cannot mistake it for an
        // older request in the previous service's retained document.
        let document = format!(
            "{document}:{}:{}",
            activation_generation.get_value(),
            provider.namespace
        );
        pending.set_value(Some((next, source, scope, document, project)));
        if running.replace(true) {
            return;
        }
        let running = running.clone();
        leptos::task::spawn_local(async move {
            while !pending.is_disposed() {
                let Some((request, source, scope, document, project)) =
                    pending.try_update_value(Option::take).flatten()
                else {
                    break;
                };
                let owner_current = move || {
                    auth.generation.try_get_untracked() == Some(origin.0)
                        && projects.active_project.try_get_untracked() == Some(Some(project))
                };
                let current = || {
                    owner_current()
                        && !ticket.is_disposed()
                        && ticket.get_value() == request
                        && snapshot
                            .try_with_untracked(|current| {
                                current.0 == source.0 && Arc::ptr_eq(&current.1, &source.1)
                            })
                            .unwrap_or(false)
                };
                if !current() {
                    continue;
                }
                active.set_value(Some(source.clone()));
                let client = if let Some(client) = cached.get_value() {
                    Ok(client)
                } else {
                    let expected = scope.clone();
                    let generation = activation_generation.get_value();
                    ProjectLanguageService::new(
                        api,
                        host,
                        project,
                        scope.0.clone(),
                        scope.5.clone(),
                        move || {
                            owner_current()
                                && activation_generation.try_get_value() == Some(generation)
                                && activation
                                    .try_with_value(|scope| scope.as_ref() == Some(&expected))
                                    .unwrap_or(false)
                                && actions.providers_current(&expected.4, &expected.5)
                                && host.revision() == Some(expected.1)
                                && workspace.active_session.try_get_untracked() == Some(expected.2)
                                && active
                                    .try_with_value(|active| {
                                        active.as_ref().is_some_and(|active| {
                                            snapshot
                                                .try_with_untracked(|current| {
                                                    current.0 == active.0
                                                        && Arc::ptr_eq(&current.1, &active.1)
                                                })
                                                .unwrap_or(false)
                                                && actions.provider_current(
                                                    &active.0,
                                                    if complete { &active.1 } else { "" },
                                                    Some(&expected.0),
                                                )
                                        })
                                    })
                                    .unwrap_or(false)
                        },
                    )
                    .await
                };
                if !current() {
                    continue;
                }
                let Ok(client) = client else { continue };
                cached.set_value(Some(client.clone()));
                let output = client
                    .analyze(LanguageDocumentRequest {
                        document,
                        path: source.0.clone(),
                        revision: request,
                        source: source.1.clone(),
                        tab_width: 4,
                    })
                    .await;
                if !current() {
                    continue;
                }
                if let Ok(LanguageOutput::Analyzed(response)) = output
                    && matches!(response.status, AnalysisStatus::Ready { .. })
                    && let Some(data) = response.analysis
                    && let Ok(analysis) = SyntaxAnalysis::from_language_data(
                        source.1.clone(),
                        &scope.0.definition.id,
                        data,
                        client.definitions(),
                    )
                {
                    result.set(Some((scope, source, Arc::new(analysis))));
                }
            }
            running.set(false);
        });
    });
    Signal::derive(move || {
        actions.registry.track();
        actions.preferences.track();
        let loaded = actions.ready.get();
        let retry = state.retry.get();
        host.track_revision();
        let owner = (auth.generation.get(), projects.active_project.get());
        let session = workspace.active_session.get();
        snapshot.track();
        result.with(|result| {
            result
                .as_ref()
                .filter(|(scope, source, _)| {
                    loaded
                        && owner == origin
                        && session == scope.2
                        && retry == scope.3
                        && actions.providers_current(&scope.4, &scope.5)
                        && host.revision() == Some(scope.1)
                        && snapshot.with(|current| {
                            current.0 == source.0 && Arc::ptr_eq(&current.1, &source.1)
                        })
                        && actions.provider_current(
                            &source.0,
                            if complete { &source.1 } else { "" },
                            Some(&scope.0),
                        )
                })
                .map(|(_, _, analysis)| analysis.clone())
        })
    })
}
