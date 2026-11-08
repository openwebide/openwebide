//! Editor code intelligence above the shared language facade and both transports.
use super::{EditorActions, EditorPresentationScope};
use crate::{backend::Api, plugin_actions::ProjectLanguageService, project_host::ProjectHost};
use futures::future::{AbortHandle, Abortable};
use leptos::{prelude::*, task::spawn_local};
use openwebide_agent::plugins::languages::LanguageDocumentRequest;
use openwebide_core::{
    editor::{IndentStyle, Selection},
    plugins::languages::{
        LanguagePreferences, LanguageRegistry, LanguageSelection, RegisteredLanguage,
        intelligence::{
            self as protocol, Diagnostic, Query, Response,
            results::{self, TextEdit},
        },
    },
};
use std::sync::Arc;
mod edits;
pub use edits::IntelligenceFile;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceScope {
    project: i64,
    account: u64,
    session: Option<i64>,
    host: u64,
    retry: u64,
    provider: RegisteredLanguage,
    registry: LanguageRegistry,
    preferences: LanguagePreferences,
}
#[derive(Clone)]
struct Snapshot {
    presentation: EditorPresentationScope,
    revision: u64,
    source: Arc<String>,
    selection: Selection,
    service: ServiceScope,
}

#[derive(Clone, Debug)]
pub enum IntelligenceCommand {
    Hover,
    Completion,
    Suggest,
    HoverAt(usize),
    InsertSuggestion(String),
    Definition,
    References,
    Symbols,
    Rename(String),
    Formatting,
    CodeActions,
    Query(Query),
}

impl IntelligenceCommand {
    fn background(&self) -> bool {
        matches!(
            self,
            Self::Query(Query::Synchronize | Query::Saved | Query::Close)
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Presentation {
    Dialog,
    Suggestions,
    Hover,
    Insert,
}

#[derive(Clone, Copy)]
pub struct EditorIntelligence {
    pub available: Memo<bool>,
    identity: Memo<Option<ServiceScope>>,
    pub context_revision: Memo<u64>,
    request_presentation: StoredValue<Presentation>,
    queued: RwSignal<Option<(Snapshot, IntelligenceCommand)>>,
    pub review: RwSignal<Vec<IntelligenceFile>>,
    pub busy: RwSignal<bool>,
    pub rename_open: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    pub diagnostics: RwSignal<Vec<Diagnostic>>,
    diagnostics_source: StoredValue<Option<(EditorPresentationScope, u64)>>,
    pub result: RwSignal<Option<(Query, Response)>>,
    pub popup: RwSignal<Option<(Query, Response)>>,
    pub popup_busy: RwSignal<bool>,
    pub popup_error: RwSignal<Option<String>>,
    pub dismiss_popup: Callback<()>,
    pub request: Callback<IntelligenceCommand>,
    pub dismiss: Callback<()>,
    pub apply_text: Callback<Vec<TextEdit>>,
    pub navigate: Callback<protocol::results::Target>,
    pub(crate) editor: EditorActions,
    navigation: RwSignal<Option<(EditorPresentationScope, u64, usize)>>,
    workspace_actions: Option<crate::state_actions::workspace::WorkspaceActions>,
    snapshot: StoredValue<Option<Snapshot>>,
}

impl EditorActions {
    pub fn install_intelligence(self, read_only: Signal<bool>) -> Option<EditorIntelligence> {
        let (state, actions, api, host) = (
            self.languages?,
            self.language_actions?,
            use_context::<Api>()?,
            use_context::<ProjectHost>()?,
        );
        let identity = Memo::new(move |_| {
            actions.ready.get().then_some(())?;
            let presentation = self.presentation_scope()?;
            self.workspace.active_session.track();
            host.track_revision();
            let retry = state.retry.get();
            let registry = actions.registry.get();
            let preferences = actions.preferences.get();
            let selected = self.workspace.content.with(|source| {
                match registry.select_server(&presentation.path, source, None, &preferences) {
                    LanguageSelection::Selected(provider) => Some(provider.clone()),
                    _ => None,
                }
            })?;
            Some(ServiceScope {
                project: presentation.project,
                account: presentation.account,
                session: self.workspace.active_session.get_untracked(),
                host: host.revision()?,
                retry,
                provider: selected,
                registry,
                preferences,
            })
        });
        let context_revision = Memo::new(move |previous: Option<&u64>| {
            identity.track();
            previous.map_or(0, |value| value.wrapping_add(1))
        });
        let available = Memo::new(move |_| {
            identity.get().is_some()
                && !self.workspace.editor_loading.get()
                && self.capacity.get().is_none()
                && self.workspace.content.with(|source| {
                    source.len() <= openwebide_core::plugins::languages::MAX_SOURCE_BYTES
                })
        });
        let review = RwSignal::new(Vec::new());
        let navigation = RwSignal::new(None);
        let busy = RwSignal::new(false);
        let rename_open = RwSignal::new(false);
        let error = RwSignal::new(None);
        let diagnostics = RwSignal::new(Vec::new());
        let diagnostics_source = StoredValue::new(None);
        let result = RwSignal::new(None);
        let popup = RwSignal::new(None::<(Query, Response)>);
        let popup_busy = RwSignal::new(false);
        let popup_error = RwSignal::new(None);
        let request_presentation = StoredValue::new(Presentation::Dialog);
        let snapshot = StoredValue::new(None::<Snapshot>);
        let service = StoredValue::new_local(None::<(ServiceScope, ProjectLanguageService)>);
        let pending = StoredValue::new(None::<AbortHandle>);
        let automatic_abort = StoredValue::new(None::<AbortHandle>);
        let automatic_running = RwSignal::new(false);
        let queued = RwSignal::new(None::<(Snapshot, IntelligenceCommand)>);
        let ticket = StoredValue::new(0_u64);
        let settled = RwSignal::new(0_u64);
        let previous_dirty = StoredValue::new(None::<(Option<(i64, String)>, bool)>);
        let current = move |captured: &Snapshot| {
            self.presentation_scope().as_ref() == Some(&captured.presentation)
                && self.workspace.editor_source_revision.try_get_untracked()
                    == Some(captured.revision)
                && identity.try_get_untracked().flatten().as_ref() == Some(&captured.service)
        };
        let apply_text = Callback::new(move |edits: Vec<TextEdit>| {
            let Some(captured) = snapshot.get_value().filter(current) else {
                error.set(Some("The document changed. Request the edit again.".into()));
                return;
            };
            if read_only.get_untracked()
                || self.workspace.editor_composition.get_untracked().is_some()
            {
                return;
            }
            let applied = (|| {
                let caret =
                    results::caret_after(&captured.source, &edits, captured.selection.head)?;
                let edits = results::editor_edits(&captured.source, &edits)?;
                self.cancel_native_text();
                self.typing.set(None);
                let key = (
                    captured.presentation.project,
                    captured.presentation.path.clone(),
                );
                let changed = self
                    .workspace
                    .editor_documents
                    .try_update(|documents| {
                        let document = self.document(documents, key.clone());
                        document
                            .apply(edits, vec![Selection::caret(caret)], None)
                            .map_err(|error| error.to_string())?;
                        Ok::<_, String>(Some((
                            crate::state::workspace::EditorText::from(document.shared_text()),
                            document.selections()[0],
                        )))
                    })
                    .ok_or("Editor closed")?;
                self.publish_edit(key, changed)?;
                Ok::<_, String>(())
            })();
            match applied {
                Ok(()) => result.set(None),
                Err(message) => error.set(Some(message)),
            }
        });
        let dismiss_popup = Callback::new(move |()| {
            queued.update(|queued| {
                if queued
                    .as_ref()
                    .is_some_and(|(_, command)| !command.background())
                {
                    *queued = None;
                }
            });
            popup.set(None);
            popup_error.set(None);
            if request_presentation.get_value() != Presentation::Dialog {
                // Invalidate presentation immediately. Let the single automatic
                // transport request finish so a slow host cannot accumulate work.
                pending.set_value(None);
                ticket.update_value(|value| *value = value.wrapping_add(1));
                popup_busy.set(false);
            }
        });
        let dismiss = Callback::new(move |()| {
            dismiss_popup.run(());
            result.set(None);
            rename_open.set(false);
            error.set(None);
        });
        let request = Callback::new(move |command: IntelligenceCommand| {
            let mode = match &command {
                IntelligenceCommand::Suggest => Presentation::Suggestions,
                IntelligenceCommand::HoverAt(_) => Presentation::Hover,
                IntelligenceCommand::InsertSuggestion(_) => Presentation::Insert,
                _ => Presentation::Dialog,
            };
            let automatic = mode != Presentation::Dialog;
            if automatic
                && (busy.get_untracked()
                    || result.with_untracked(Option::is_some)
                    || review.with_untracked(|files: &Vec<IntelligenceFile>| !files.is_empty())
                    || rename_open.get_untracked())
            {
                return;
            }
            if mode == Presentation::Hover
                && (popup.with_untracked(|value| {
                    value
                        .as_ref()
                        .is_some_and(|(query, _)| matches!(query, Query::Completion { .. }))
                }) || (popup_busy.get_untracked()
                    && request_presentation.get_value() != Presentation::Hover))
            {
                return;
            }
            if !available.get_untracked() {
                return;
            }
            let Some(scope) = identity.get_untracked() else {
                return;
            };
            if self.workspace.editor_loading.get_untracked()
                || self.workspace.editor_composition.get_untracked().is_some()
                || self.limit().is_some()
            {
                return;
            }
            let Some(presentation) = self.presentation_scope() else {
                return;
            };
            let captured = Snapshot {
                presentation,
                revision: self.workspace.editor_source_revision.get_untracked(),
                source: self
                    .workspace
                    .content
                    .with_untracked(crate::state::workspace::EditorText::shared),
                selection: self.current_selection().unwrap_or(Selection::caret(0)),
                service: scope.clone(),
            };
            if automatic
                && (captured.selection.anchor != captured.selection.head
                    || self.selection_count() > 1)
            {
                return;
            }
            if automatic && read_only.get_untracked() && mode != Presentation::Hover {
                return;
            }
            if automatic && automatic_running.get_untracked() {
                queued.set(Some((captured, command)));
                popup_busy.set(true);
                return;
            }
            let offset = match &command {
                IntelligenceCommand::HoverAt(offset) => *offset,
                _ => captured.selection.head,
            };
            let position = match protocol::position(&captured.source, offset) {
                Ok(position) => position,
                Err(message) => {
                    if !automatic {
                        error.set(Some(message));
                    }
                    return;
                }
            };
            let query = match command {
                IntelligenceCommand::Hover => Query::Hover { position },
                IntelligenceCommand::Completion | IntelligenceCommand::Suggest => {
                    Query::Completion { position }
                }
                IntelligenceCommand::HoverAt(_) => Query::Hover { position },
                IntelligenceCommand::InsertSuggestion(id) => Query::ResolveCompletion { id },
                IntelligenceCommand::Definition => Query::Definition { position },
                IntelligenceCommand::References => Query::References { position },
                IntelligenceCommand::Symbols => Query::Symbols,
                IntelligenceCommand::Rename(new_name) => Query::Rename { position, new_name },
                IntelligenceCommand::Formatting => Query::Formatting {
                    tab_width: u32::try_from(self.rules_untracked().indentation.tab_width())
                        .unwrap_or(4),
                    insert_spaces: self.rules_untracked().indentation.style == IndentStyle::Spaces,
                },
                IntelligenceCommand::CodeActions => {
                    let range = captured.selection.range();
                    let Ok(start) = protocol::position(&captured.source, range.start) else {
                        return;
                    };
                    let Ok(end) = protocol::position(&captured.source, range.end) else {
                        return;
                    };
                    Query::CodeActions {
                        range: protocol::Range { start, end },
                    }
                }
                IntelligenceCommand::Query(query) => query,
            };
            let background = matches!(query, Query::Synchronize | Query::Saved | Query::Close);
            if read_only.get_untracked()
                && matches!(
                    query,
                    Query::Completion { .. }
                        | Query::ResolveCompletion { .. }
                        | Query::Rename { .. }
                        | Query::Formatting { .. }
                        | Query::CodeActions { .. }
                        | Query::ResolveAction { .. }
                )
            {
                return;
            }
            if background && automatic_running.get_untracked() && !busy.get_untracked() {
                if queued.with_untracked(Option::is_none) {
                    queued.set(Some((captured, IntelligenceCommand::Query(query))));
                }
                return;
            }
            if background && (busy.get_untracked() || popup_busy.get_untracked()) {
                return;
            }
            if request_presentation.get_value() == Presentation::Dialog
                && let Some(previous) = pending.get_value()
            {
                previous.abort();
            }
            if !automatic {
                queued.set(None);
            }
            ticket.update_value(|ticket| *ticket = ticket.wrapping_add(1));
            let request_ticket = ticket.get_value();
            let (abort, registration) = AbortHandle::new_pair();
            if automatic {
                automatic_running.set(true);
                automatic_abort.set_value(Some(abort.clone()));
            }
            pending.set_value(Some(abort));
            request_presentation.set_value(mode);
            if !background {
                busy.set(!automatic);
                popup_busy.set(automatic);
                popup_error.set(None);
                if automatic && mode != Presentation::Insert {
                    popup.set(None);
                }
                if !automatic {
                    popup.set(None);
                    error.set(None);
                    result.set(None);
                    review.set(Vec::new());
                }
            }
            spawn_local(async move {
                let operation = async {
                    let cached = service.with_value(|cached| {
                        cached
                            .as_ref()
                            .filter(|(identity, _)| identity == &scope)
                            .map(|(_, service)| service.clone())
                    });
                    let client = state.intelligence_client.with_value(|client| {
                        client
                            .as_ref()
                            .filter(|(account, _)| *account == scope.account)
                            .map(|(_, client)| client.clone())
                    });
                    let client = if let Some(client) = client {
                        client
                    } else {
                        let mut nonce = [0_u8; 16];
                        web_sys::window()
                            .ok_or("Browser is unavailable")?
                            .crypto()
                            .map_err(|_| "Language session identity is unavailable")?
                            .get_random_values_with_u8_array(&mut nonce)
                            .map_err(|_| "Language session identity is unavailable")?;
                        let client = nonce
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>();
                        state
                            .intelligence_client
                            .set_value(Some((scope.account, client.clone())));
                        state.intelligence_documents.set_value(Default::default());
                        client
                    };
                    let client_service = if let Some(service) = cached {
                        service
                    } else {
                        let expected = scope.clone();
                        let service_current = move || {
                            identity.try_get_untracked().flatten().as_ref() == Some(&expected)
                        };
                        let created = ProjectLanguageService::new(
                            api,
                            host,
                            scope.project,
                            scope.provider.clone(),
                            scope.preferences.clone(),
                            service_current,
                        )
                        .await?
                        .with_sequence(state.intelligence_sequence.get_value());
                        if !current(&captured) {
                            return Err("Editor context changed".into());
                        }
                        service.set_value(Some((scope.clone(), created.clone())));
                        created
                    };
                    // Every open buffer using this provider participates in
                    // cross-file queries, including unsaved inactive tabs.
                    let other_sources = self
                        .workspace
                        .editor_tabs
                        .with_untracked(|tabs| {
                            tabs.get(&scope.project).cloned().unwrap_or_default()
                        })
                        .into_iter()
                        .filter(|path| path != &captured.presentation.path)
                        .filter_map(|path| {
                            let key = (scope.project, path.clone());
                            let source = self
                                .workspace
                                .editor_buffers
                                .with_untracked(|buffers| {
                                    buffers.get(&key).map(|buffer| buffer.content.shared())
                                })
                                .or_else(|| {
                                    self.workspace.editor_documents.with_untracked(|documents| {
                                        documents
                                            .get(&key)
                                            .map(openwebide_core::editor::Document::shared_text)
                                    })
                                })?;
                            if source.len() > openwebide_core::plugins::languages::MAX_SOURCE_BYTES
                            {
                                return None;
                            }
                            let LanguageSelection::Selected(provider) = scope
                                .registry
                                .select_server(&path, &source, None, &scope.preferences)
                            else {
                                return None;
                            };
                            scope.provider.shares_server(provider).then_some((
                                path,
                                source,
                                provider.clone(),
                            ))
                        })
                        .collect::<Vec<_>>();
                    let configuration = scope
                        .provider
                        .definition
                        .features
                        .language_server
                        .as_ref()
                        .ok_or("The language server is unavailable")?
                        .shared_configuration();
                    let document_key = (
                        scope.project,
                        scope.provider.plugin_namespace.clone(),
                        serde_json::to_string(&configuration).map_err(|error| error.to_string())?,
                    );
                    let mut paths = other_sources
                        .iter()
                        .map(|(path, _, provider)| (path.clone(), provider.definition.id.clone()))
                        .collect::<std::collections::BTreeMap<_, _>>();
                    paths.insert(
                        captured.presentation.path.clone(),
                        scope.provider.definition.id.clone(),
                    );
                    let closed = state.intelligence_documents.with_value(|documents| {
                        documents
                            .get(&document_key)
                            .filter(|documents| documents.digest == scope.provider.digest)
                            .map(|documents| {
                                documents
                                    .paths
                                    .iter()
                                    .filter(|(path, _)| !paths.contains_key(*path))
                                    .map(|(path, provider)| (path.clone(), provider.clone()))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    });
                    for (path, language) in closed {
                        let provider = scope
                            .registry
                            .providers
                            .iter()
                            .find(|provider| {
                                provider.definition.id == language
                                    && scope.provider.shares_server(provider)
                            })
                            .ok_or("The previous document language is no longer available")?;
                        client_service
                            .for_server_language(provider.clone())?
                            .intelligence(
                                &client,
                                LanguageDocumentRequest {
                                    document: openwebide_core::plugins::languages::source_digest(
                                        &path,
                                    ),
                                    path,
                                    source: Arc::new(String::new()),
                                    revision: captured.revision,
                                    tab_width: self.rules_untracked().indentation.tab_width(),
                                },
                                Query::Close,
                            )
                            .await?;
                        if !current(&captured) {
                            return Err("Editor context changed".into());
                        }
                    }
                    state.intelligence_documents.update_value(|documents| {
                        documents.retain(|(project, provider, _), previous| {
                            *project != scope.project
                                || provider != &scope.provider.plugin_namespace
                                || previous.digest == scope.provider.digest
                        });
                        documents.insert(
                            document_key,
                            crate::state::languages::IntelligenceDocuments {
                                digest: scope.provider.digest.clone(),
                                paths,
                            },
                        );
                    });
                    for (path, source, provider) in other_sources {
                        client_service
                            .for_server_language(provider)?
                            .intelligence(
                                &client,
                                LanguageDocumentRequest {
                                    document: openwebide_core::plugins::languages::source_digest(
                                        &path,
                                    ),
                                    path,
                                    source,
                                    revision: captured.revision,
                                    tab_width: self.rules_untracked().indentation.tab_width(),
                                },
                                Query::Synchronize,
                            )
                            .await?;
                        if !current(&captured) {
                            return Err("Editor context changed".into());
                        }
                    }
                    client_service
                        .intelligence(
                            &client,
                            LanguageDocumentRequest {
                                document: openwebide_core::plugins::languages::source_digest(
                                    &captured.presentation.path,
                                ),
                                path: captured.presentation.path.clone(),
                                revision: captured.revision,
                                source: captured.source.clone(),
                                tab_width: self.rules_untracked().indentation.tab_width(),
                            },
                            query.clone(),
                        )
                        .await
                };
                let response = Abortable::new(operation, registration).await;
                if automatic {
                    let _ = automatic_running.try_set(false);
                    let _ = automatic_abort.try_update_value(|handle| *handle = None);
                }
                if ticket.try_get_value() != Some(request_ticket) || !current(&captured) {
                    return;
                }
                pending.set_value(None);
                busy.set(false);
                popup_busy.set(false);
                settled.update(|generation| *generation = generation.wrapping_add(1));
                match response {
                    Ok(Ok(response)) => {
                        if response.diagnostics_ready {
                            diagnostics_source.set_value(Some((
                                captured.presentation.clone(),
                                captured.revision,
                            )));
                            diagnostics.set(response.diagnostics.clone());
                        }
                        if !background {
                            if self.current_selection().unwrap_or_default() != captured.selection {
                                return;
                            }
                            if mode == Presentation::Insert {
                                let edits = results::completion_edits(
                                    &captured.source,
                                    position,
                                    &response.result,
                                );
                                snapshot.set_value(Some(captured));
                                match edits {
                                    Ok(edits) => {
                                        apply_text.run(edits);
                                        if let Some(message) = error.get_untracked() {
                                            error.set(None);
                                            popup_error.set(Some(message));
                                        } else {
                                            popup.set(None);
                                        }
                                    }
                                    Err(message) => popup_error.set(Some(message)),
                                }
                            } else {
                                snapshot.set_value(Some(captured));
                                if automatic {
                                    let present = match &query {
                                        Query::Completion { .. } => response
                                            .result
                                            .as_array()
                                            .is_some_and(|items| !items.is_empty()),
                                        Query::Hover { .. } => results::hover(&response.result)
                                            .ok()
                                            .flatten()
                                            .is_some(),
                                        _ => false,
                                    };
                                    popup.set(present.then_some((query, response)));
                                } else {
                                    result.set(Some((query, response)));
                                }
                            }
                        }
                    }
                    Ok(Err(message)) => {
                        if mode == Presentation::Insert {
                            popup_error.set(Some(message));
                        } else if !automatic {
                            error.set(Some(message));
                        } else {
                            popup.set(None);
                        }
                    }
                    Err(_) => {}
                }
            });
        });
        Effect::new(move |_| {
            if automatic_running.get() {
                return;
            }
            if let Some((captured, command)) = queued.get() {
                queued.set(None);
                if current(&captured)
                    && (command.background()
                        || self.current_selection() == Some(captured.selection))
                {
                    request.run(command);
                }
            }
        });
        let workspace_actions = use_context::<crate::state_actions::workspace::WorkspaceActions>();
        let navigate = Callback::new(move |target: results::Target| {
            if let Err(message) = target.validate() {
                error.set(Some(message));
                return;
            }
            let Some(captured) = snapshot.get_value().filter(current) else {
                return;
            };
            if let Some(resource) = target.resource {
                request.run(IntelligenceCommand::Query(Query::ReadResource {
                    id: resource,
                }));
                return;
            }
            let (Some(path), Some(actions)) = (target.path, workspace_actions) else {
                return;
            };
            result.set(None);
            actions.open_with_completion.run((
                path.clone(),
                Callback::new(move |opened: Result<(), String>| {
                    if self.capacity.try_get_untracked().is_none()
                        || self.account_generation() != captured.presentation.account
                        || !self.is_current(captured.presentation.project, &path)
                    {
                        return;
                    }
                    if let Err(message) = opened {
                        error.set(Some(message));
                        return;
                    }
                    let offsets = self.workspace.content.with_untracked(|source| {
                        if target.source_digest.as_deref()
                            != Some(&openwebide_core::plugins::languages::source_digest(source))
                        {
                            return Err(
                                "The destination changed. Request navigation again.".to_string()
                            );
                        }
                        target.range.offsets(source)
                    });
                    match offsets {
                        Ok(range) => {
                            if let Some(presentation) = self.presentation_scope() {
                                navigation.set(Some((
                                    presentation,
                                    self.source_revision(),
                                    range.start,
                                )));
                            }
                        }
                        Err(message) => error.set(Some(message)),
                    }
                }),
            ));
        });
        Effect::new(move |_| {
            identity.track();
            self.workspace.editor_source_revision.track();
            self.workspace.editor_composition.track();
            let dirty = self.workspace.dirty.get();
            let key = self
                .workspace
                .active_project
                .get()
                .zip(self.workspace.open_file.get());
            let saved = previous_dirty.with_value(|previous| {
                previous.as_ref().is_some_and(|(previous_key, was_dirty)| {
                    previous_key == &key && *was_dirty && !dirty
                })
            });
            previous_dirty.set_value(Some((key, dirty)));
            if request_presentation.get_value() == Presentation::Dialog
                && let Some(previous) = pending.get_value()
            {
                previous.abort();
            }
            queued.set(None);
            ticket.update_value(|ticket| *ticket = ticket.wrapping_add(1));
            busy.set(false);
            popup_busy.set(false);
            popup.set(None);
            popup_error.set(None);
            result.set(None);
            error.set(None);
            diagnostics.set(Vec::new());
            diagnostics_source.set_value(None);
            snapshot.set_value(None);
            review.set(Vec::new());
            let expected = ticket.get_value();
            if !available.get_untracked() {
                service.set_value(None);
                return;
            }
            spawn_local(async move {
                crate::util::sleep_ms(400).await;
                if ticket.try_get_value() == Some(expected) {
                    request.run(IntelligenceCommand::Query(if saved {
                        Query::Saved
                    } else {
                        Query::Synchronize
                    }));
                }
            });
        });
        on_cleanup(move || {
            if let Some(Some(automatic)) = automatic_abort.try_get_value() {
                automatic.abort();
            }
            if let Some(Some(pending)) = pending.try_get_value() {
                pending.abort();
            }
        });
        Effect::new(move |_| {
            settled.track();
            let generation = ticket.get_value();
            if !available.get() {
                return;
            }
            spawn_local(async move {
                crate::util::sleep_ms(5000).await;
                if ticket.try_get_value() == Some(generation)
                    && !busy.try_get_untracked().unwrap_or(true)
                    && !popup_busy.try_get_untracked().unwrap_or(true)
                {
                    request.run(IntelligenceCommand::Query(Query::Synchronize));
                }
            });
        });
        Some(EditorIntelligence {
            available,
            identity,
            context_revision,
            request_presentation,
            queued,
            review,
            busy,
            rename_open,
            error,
            diagnostics,
            diagnostics_source,
            result,
            popup,
            popup_busy,
            popup_error,
            dismiss_popup,
            request,
            dismiss,
            apply_text,
            navigate,
            editor: self,
            navigation,
            workspace_actions,
            snapshot,
        })
    }
}

impl EditorIntelligence {
    pub fn completion_active(self) -> bool {
        self.queued.with_untracked(|queued| {
            queued.as_ref().is_some_and(|(_, command)| {
                matches!(
                    command,
                    IntelligenceCommand::Suggest | IntelligenceCommand::InsertSuggestion(_)
                )
            })
        }) || self.popup.with_untracked(|value| {
            value
                .as_ref()
                .is_some_and(|(query, _)| matches!(query, Query::Completion { .. }))
        }) || (self.popup_busy.get_untracked()
            && matches!(
                self.request_presentation.get_value(),
                Presentation::Suggestions | Presentation::Insert
            ))
    }
    pub fn popup_offset(self) -> Option<usize> {
        let captured = self.snapshot.get_value().filter(|captured| {
            self.snapshot_current(captured)
                && self.editor.current_selection() == Some(captured.selection)
        })?;
        self.popup.with_untracked(|value| match value.as_ref()?.0 {
            Query::Completion { position } | Query::Hover { position } => {
                protocol::offset(&captured.source, position).ok()
            }
            _ => None,
        })
    }
    pub fn document_path(self) -> Option<String> {
        self.snapshot
            .get_value()
            .filter(|snapshot| self.snapshot_current(snapshot))
            .map(|snapshot| snapshot.presentation.path)
    }
    pub fn take_navigation(self) -> Option<usize> {
        let (scope, revision, offset) = self.navigation.get()?;
        self.navigation.set(None);
        (self.editor.presentation_scope().as_ref() == Some(&scope)
            && self.editor.source_revision() == revision)
            .then_some(offset)
    }
    pub fn navigate_diagnostic(self, range: protocol::Range) {
        if !self.diagnostics_current() {
            return;
        }
        let offset = self
            .editor
            .workspace
            .content
            .with_untracked(|source| range.offsets(source));
        if let (Ok(offset), Some(scope)) = (offset, self.editor.presentation_scope()) {
            self.navigation
                .set(Some((scope, self.editor.source_revision(), offset.start)));
        }
    }
    pub fn diagnostics_current(self) -> bool {
        self.diagnostics_source
            .try_get_value()
            .flatten()
            .is_some_and(|(scope, revision)| {
                self.editor.presentation_scope().as_ref() == Some(&scope)
                    && self
                        .editor
                        .workspace
                        .editor_source_revision
                        .try_get_untracked()
                        == Some(revision)
            })
    }
    fn snapshot_current(self, captured: &Snapshot) -> bool {
        self.editor.presentation_scope().as_ref() == Some(&captured.presentation)
            && self
                .editor
                .workspace
                .editor_source_revision
                .try_get_untracked()
                == Some(captured.revision)
            && self.identity.try_get_untracked().flatten().as_ref() == Some(&captured.service)
    }

    pub fn apply_completion(self, item: &serde_json::Value) {
        let edits = self
            .snapshot
            .get_value()
            .ok_or_else(|| "Completion expired".to_string())
            .and_then(|snapshot| {
                results::completion_edits(
                    &snapshot.source,
                    protocol::position(&snapshot.source, snapshot.selection.head)?,
                    item,
                )
            });
        match edits {
            Ok(edits) => self.apply_text.run(edits),
            Err(error) => self.error.set(Some(error)),
        }
    }
}
