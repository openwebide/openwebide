//! Editor code intelligence above the shared language facade and both transports.
use super::{EditorActions, EditorPresentationScope};
use crate::workspace_transactions::review::WorkspaceReviewFiles;
use crate::{backend::Api, plugin_actions::ProjectLanguageService, project_host::ProjectHost};
use futures::future::{AbortHandle, Abortable};
use leptos::{prelude::*, task::spawn_local};
use openwebide_agent::plugins::languages::{LanguageDocumentRequest, ProviderDiagnostics};
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
mod application;
use application::{AnalysisResult, PreparedEdit};
mod edits;
use edits::{ResourceContext, clear_workspace_review};
pub use edits::{WorkspaceResourceReview, WorkspaceTextReview};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceScope {
    project: i64,
    account: u64,
    session: Option<i64>,
    host: u64,
    retry: u64,
    repository: u64,
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
#[derive(Clone, PartialEq, Eq)]
struct RepositoryRevision {
    project: Option<i64>,
    observed: Option<(String, String)>,
    revision: u64,
}
enum IntelligenceReply {
    Server(Box<(Response, RegisteredLanguage, Option<Arc<AnalysisResult>>)>),
    Diagnostics(Vec<ProviderDiagnostics>),
}

#[derive(Clone, Debug)]
pub enum IntelligenceCommand {
    Hover,
    Completion,
    Suggest,
    HoverAt(usize),
    InsertSuggestion(String),
    Definition,
    TypeDefinition,
    Implementation,
    References,
    Symbols,
    WorkspaceSymbols(String),
    Rename(String),
    Formatting,
    RangeFormatting,
    CodeActions,
    Query(Query),
    ProviderQuery { provider: String, query: Query },
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
    pub primary_available: Memo<bool>,
    identity: Memo<Option<ServiceScope>>,
    source_revision: Memo<u64>,
    pub context_revision: Memo<u64>,
    request_presentation: StoredValue<Presentation>,
    queued: RwSignal<Option<(Snapshot, IntelligenceCommand)>>,
    pub review: RwSignal<Option<Arc<WorkspaceTextReview>>>,
    pub resources: RwSignal<Option<Arc<WorkspaceResourceReview>>>,
    review_epoch: StoredValue<u64>,
    resource_context: Option<ResourceContext>,
    pub busy: RwSignal<bool>,
    pub rename_open: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    pub diagnostics: RwSignal<Vec<Diagnostic>>,
    pub provider_diagnostics: RwSignal<Vec<ProviderDiagnostics>>,
    diagnostics_source: StoredValue<Option<(EditorPresentationScope, u64)>>,
    pub result: RwSignal<Option<(Query, Response)>>,
    pub popup: RwSignal<Option<(Query, Response)>>,
    pub popup_busy: RwSignal<bool>,
    pub popup_error: RwSignal<Option<String>>,
    pub dismiss_popup: Callback<()>,
    pub request: Callback<IntelligenceCommand>,
    pub dismiss: Callback<()>,
    pub apply_text: Callback<Vec<TextEdit>>,
    insert: Callback<serde_json::Value, Result<(), String>>,
    pub navigate: Callback<protocol::results::Target>,
    pub(crate) editor: EditorActions,
    navigation: RwSignal<Option<(EditorPresentationScope, u64, usize)>>,
    workspace_actions: Option<crate::state_actions::workspace::WorkspaceActions>,
    snapshot: StoredValue<Option<Snapshot>>,
    analysis: StoredValue<Option<Arc<AnalysisResult>>>,
    application_abort: StoredValue<Option<AbortHandle>>,
}

impl EditorActions {
    pub fn install_intelligence(self, read_only: Signal<bool>) -> Option<EditorIntelligence> {
        let (state, actions, api, host) = (
            self.languages?,
            self.language_actions?,
            use_context::<Api>()?,
            use_context::<ProjectHost>()?,
        );
        let git = use_context::<crate::state::git::GitState>();
        let presentation = Memo::new(move |_| self.presentation_scope());
        // Keep one effective inventory for validity and native synchronization.
        // Active text owns its path; retained buffers override cached documents.
        let source = Memo::new(move |_| {
            let presentation = presentation.get()?;
            let paths = self
                .workspace
                .editor_tabs
                .with(|tabs| tabs.get(&presentation.project).cloned().unwrap_or_default());
            let mut open = std::collections::BTreeMap::new();
            for path in paths.into_iter().filter(|path| path != &presentation.path) {
                let key = (presentation.project, path.clone());
                let text = self
                    .workspace
                    .editor_buffers
                    .with(|buffers| buffers.get(&key).map(|buffer| buffer.content.shared()))
                    .or_else(|| {
                        self.workspace.editor_documents.with(|documents| {
                            documents
                                .get(&key)
                                .map(openwebide_core::editor::Document::shared_text)
                        })
                    });
                if let Some(text) = text {
                    open.insert(path, text);
                }
            }
            Some((
                presentation,
                self.workspace
                    .content
                    .with(crate::state::workspace::EditorText::shared),
                open,
            ))
        });
        let source_revision = Memo::new(move |_| {
            source.with(|_| ());
            // The editing client keeps this allocator across component remounts.
            // Revisions and request IDs may have gaps; both remain monotonic.
            state.intelligence_sequence.with_value(|sequence| {
                sequence
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .saturating_add(1)
            })
        });
        let composition = Memo::new(move |_| self.workspace.editor_composition.get());
        let dirty = Memo::new(move |_| self.workspace.dirty.get());
        let repository = Memo::new(move |previous: Option<&RepositoryRevision>| {
            let project = self.workspace.active_project.get();
            let observed = git.and_then(|git| {
                git.status.with(|status| {
                    status
                        .as_ref()
                        .map(|status| (status.branch.clone(), status.commit_hash.clone()))
                })
            });
            let previous = previous.filter(|previous| previous.project == project);
            // Git status loads independently of language services. Its first
            // observation establishes a baseline; only a change from a known
            // revision invalidates results. Retain that baseline through a
            // temporary unavailable status so a later branch change is visible.
            let changed = previous.is_some_and(|previous| {
                matches!((&previous.observed, &observed), (Some(before), Some(after)) if before != after)
            });
            RepositoryRevision {
                project,
                observed: observed
                    .or_else(|| previous.and_then(|previous| previous.observed.clone())),
                revision: previous.map_or(0, |previous| {
                    previous.revision.wrapping_add(u64::from(changed))
                }),
            }
        });
        let repository = Memo::new(move |_| repository.with(|repository| repository.revision));
        let identity = Memo::new(move |_| {
            actions.ready.get().then_some(())?;
            let presentation = presentation.get()?;
            self.workspace.active_session.track();
            host.track_revision();
            let retry = state.retry.get();
            let registry = actions.registry.get();
            let preferences = actions.preferences.get();
            let selected = self.workspace.content.with(|source| {
                match registry.select_server(&presentation.path, source, None, &preferences) {
                    LanguageSelection::Selected(provider) => Some(provider.clone()),
                    _ => registry
                        .diagnostic_providers(&presentation.path, source, None, &preferences)
                        .ok()?
                        .first()
                        .map(|provider| (*provider).clone()),
                }
            })?;
            Some(ServiceScope {
                project: presentation.project,
                account: presentation.account,
                session: self.workspace.active_session.get_untracked(),
                host: host.revision()?,
                retry,
                repository: repository.get(),
                provider: selected,
                registry,
                preferences,
            })
        });
        let context_revision = Memo::new(move |previous: Option<&u64>| {
            identity.with(|_| ());
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
        let primary_available = Memo::new(move |_| {
            available.get()
                && identity.with(|scope| {
                    scope.as_ref().is_some_and(|scope| {
                        scope
                            .provider
                            .definition
                            .features
                            .language_server
                            .as_ref()
                            .is_some_and(|server| server.role.is_language())
                    })
                })
        });
        let review = RwSignal::new(None::<Arc<WorkspaceTextReview>>);
        let resources = RwSignal::new(None::<Arc<WorkspaceResourceReview>>);
        let review_epoch = StoredValue::new(0_u64);
        let resource_context = ResourceContext::from_context();
        let navigation = RwSignal::new(None);
        let busy = RwSignal::new(false);
        let rename_open = RwSignal::new(false);
        let error = RwSignal::new(None);
        let diagnostics = RwSignal::new(Vec::new());
        let provider_diagnostics = RwSignal::new(Vec::<ProviderDiagnostics>::new());
        let diagnostics_source = StoredValue::new(None);
        let result = RwSignal::new(None);
        let popup = RwSignal::new(None::<(Query, Response)>);
        let popup_busy = RwSignal::new(false);
        let popup_error = RwSignal::new(None);
        let request_presentation = StoredValue::new(Presentation::Dialog);
        let snapshot = StoredValue::new(None::<Snapshot>);
        let analysis = StoredValue::new(None::<Arc<AnalysisResult>>);
        let workspace_files =
            StoredValue::new(None::<(Snapshot, Result<Arc<WorkspaceReviewFiles>, String>)>);
        let application_abort = StoredValue::new(None::<AbortHandle>);
        let result_provider = StoredValue::new(None::<String>);
        let completion_followup = RwSignal::new(None::<Snapshot>);
        let service = StoredValue::new_local(None::<(ServiceScope, ProjectLanguageService)>);
        let pending = StoredValue::new(None::<AbortHandle>);
        let automatic_abort = StoredValue::new(None::<AbortHandle>);
        let automatic_running = RwSignal::new(false);
        let queued = RwSignal::new(None::<(Snapshot, IntelligenceCommand)>);
        let ticket = StoredValue::new(0_u64);
        let settled = RwSignal::new(0_u64);
        let previous_dirty = StoredValue::new(None::<(Option<(i64, String)>, bool)>);
        let current = move |captured: &Snapshot| {
            untrack(|| self.presentation_scope()).as_ref() == Some(&captured.presentation)
                && source_revision.try_get_untracked() == Some(captured.revision)
                && identity.try_get_untracked().flatten().as_ref() == Some(&captured.service)
        };
        let publish_diagnostics = Callback::new(
            move |(captured, reports, replace): (Snapshot, Vec<ProviderDiagnostics>, bool)| {
                if !current(&captured) {
                    return;
                }
                let same = diagnostics_source.with_value(|previous| {
                    previous.as_ref().is_some_and(|(scope, revision)| {
                        scope == &captured.presentation && *revision == captured.revision
                    })
                });
                let previous = if same && !replace {
                    provider_diagnostics.get_untracked()
                } else {
                    vec![]
                };
                let reports =
                    openwebide_agent::plugins::languages::merge_diagnostics(previous, reports);
                let combined = reports
                    .iter()
                    .filter(|report| report.ready)
                    .flat_map(|report| report.diagnostics.iter().cloned())
                    .collect();
                diagnostics_source.set_value(Some((captured.presentation, captured.revision)));
                provider_diagnostics.set(reports);
                diagnostics.set(combined);
            },
        );
        let apply = Callback::new(
            move |(edits, snippet): (
                Vec<TextEdit>,
                Option<openwebide_core::editor::snippet::Snippet>,
            )| {
                let Some(captured) = snapshot.get_value().filter(current) else {
                    error.set(Some("The document changed. Request the edit again.".into()));
                    return false;
                };
                if read_only.get_untracked()
                    || self.workspace.editor_composition.get_untracked().is_some()
                {
                    return false;
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
                            let original = self.document(documents, key.clone());
                            let mut document = original.clone();
                            let selections = snippet
                                .as_ref()
                                .and_then(|snippet| {
                                    snippet
                                        .fields
                                        .iter()
                                        .find(|(index, _)| **index != 0)
                                        .or_else(|| snippet.fields.get_key_value(&0))
                                })
                                .map_or_else(
                                    || vec![Selection::caret(caret)],
                                    |(_, ranges)| {
                                        ranges
                                            .iter()
                                            .map(|range| Selection {
                                                anchor: range.start,
                                                head: range.end,
                                            })
                                            .collect()
                                    },
                                );
                            document
                                .apply(edits, selections, None)
                                .map_err(|error| error.to_string())?;
                            if let Some(snippet) = snippet {
                                document
                                    .start_snippet(snippet)
                                    .map_err(|error| error.to_string())?;
                            }
                            let changed = Some((
                                crate::state::workspace::EditorText::from(document.shared_text()),
                                document.selections()[0],
                            ));
                            *original = document;
                            Ok::<_, String>(changed)
                        })
                        .ok_or("Editor closed")?;
                    self.publish_edit(key, changed)?;
                    Ok::<_, String>(())
                })();
                match applied {
                    Ok(()) => {
                        result.set(None);
                        analysis.set_value(None);
                        true
                    }
                    Err(message) => {
                        error.set(Some(message));
                        false
                    }
                }
            },
        );
        let apply_validated = Callback::new(
            move |(captured, prepared, mode): (Arc<AnalysisResult>, PreparedEdit, Presentation)| {
                if busy.get_untracked() || popup_busy.get_untracked() {
                    return Err("A language operation is already pending".to_string());
                }
                let selected = analysis
                    .get_value()
                    .is_some_and(|selected| Arc::ptr_eq(&selected, &captured));
                if !selected
                    || !current(&captured.snapshot)
                    || self.current_selection() != Some(captured.snapshot.selection)
                {
                    return Err("The language result changed. Request it again.".into());
                }
                if read_only.get_untracked()
                    || self.workspace.editor_composition.get_untracked().is_some()
                {
                    return Err("The edit cannot be applied in the current editor".into());
                }
                // A late background reply must not reset this operation's busy
                // state or replace its result while validation is pending.
                if let Some(previous) = pending.get_value() {
                    previous.abort();
                }
                pending.set_value(None);
                queued.set(None);
                ticket.update_value(|ticket| *ticket = ticket.wrapping_add(1));
                let expected = ticket.get_value();
                if let Some(previous) = application_abort.get_value() {
                    previous.abort();
                }
                let (abort, registration) = AbortHandle::new_pair();
                application_abort.set_value(Some(abort));
                if mode == Presentation::Insert {
                    popup_busy.set(true);
                    popup_error.set(None);
                } else {
                    busy.set(true);
                    error.set(None);
                }
                spawn_local(async move {
                    let validated = Abortable::new(captured.validate(), registration).await;
                    if ticket.try_get_value() != Some(expected) {
                        return;
                    }
                    application_abort.set_value(None);
                    if mode == Presentation::Insert {
                        popup_busy.set(false);
                    } else {
                        busy.set(false);
                    }
                    settled.update(|value| *value = value.wrapping_add(1));
                    let selected = analysis
                        .try_get_value()
                        .flatten()
                        .is_some_and(|selected| Arc::ptr_eq(&selected, &captured));
                    if !selected
                        || !current(&captured.snapshot)
                        || self.current_selection() != Some(captured.snapshot.selection)
                    {
                        return;
                    }
                    let failed = |message| {
                        if mode == Presentation::Insert {
                            error.set(None);
                            popup_error.set(Some(message));
                        } else {
                            error.set(Some(message));
                        }
                    };
                    match validated {
                        Ok(Ok(())) => {}
                        Ok(Err(message)) => {
                            failed(message);
                            return;
                        }
                        Err(_) => return,
                    }
                    if !apply.run((prepared.edits, prepared.snippet)) {
                        failed(error.get_untracked().unwrap_or_else(|| {
                            "The edit cannot be applied in the current editor".into()
                        }));
                        return;
                    }
                    popup.set(None);
                    if prepared.suggest
                        && let (Some(presentation), Some(service)) =
                            (self.presentation_scope(), identity.get_untracked())
                    {
                        completion_followup.set(Some(Snapshot {
                            presentation,
                            revision: source_revision.get_untracked(),
                            source: self.source().shared(),
                            selection: self.current_selection().unwrap_or_default(),
                            service,
                        }));
                    }
                });
                Ok::<_, String>(())
            },
        );
        let apply_text = Callback::new(move |edits| {
            let prepared = (|| {
                let captured = analysis
                    .get_value()
                    .ok_or("This formatting result cannot be applied. Request it again.")?;
                let prepared = captured.formatting(edits)?;
                apply_validated.run((captured, prepared, Presentation::Dialog))
            })();
            if let Err(message) = prepared {
                error.set(Some(message));
            }
        });
        // Ok means that the validated insertion was queued. Its owned task
        // reports validation failure and closes the popup only after publication.
        let insert = Callback::new(move |item: serde_json::Value| {
            let captured = analysis
                .get_value()
                .ok_or("This completion cannot be applied. Request it again.")?;
            let prepared = captured.completion(&item)?;
            apply_validated.run((captured, prepared, request_presentation.get_value()))
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
                if let Some(previous) = application_abort.get_value() {
                    previous.abort();
                }
                application_abort.set_value(None);
                analysis.set_value(None);
                // Invalidate presentation immediately. Let the single automatic
                // transport request finish so a slow host cannot accumulate work.
                pending.set_value(None);
                ticket.update_value(|value| *value = value.wrapping_add(1));
                popup_busy.set(false);
                // Presentation cancellation must not stop the current source's
                // periodic diagnostics, including a still-pending server report.
                settled.update(|generation| *generation = generation.wrapping_add(1));
            }
        });
        let dismiss = Callback::new(move |()| {
            if self
                .workspace
                .active_project
                .get_untracked()
                .is_some_and(|project| self.workspace.editor_operation_active_untracked(project))
            {
                return;
            }
            clear_workspace_review(review, resources, review_epoch, resource_context);
            dismiss_popup.run(());
            if let Some(previous) = application_abort.get_value() {
                previous.abort();
            }
            application_abort.set_value(None);
            analysis.set_value(None);
            workspace_files.set_value(None);
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
                    || review.with_untracked(Option::is_some)
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
            if self
                .workspace
                .active_project
                .get_untracked()
                .is_some_and(|project| self.workspace.editor_operation_active_untracked(project))
            {
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
                revision: source_revision.get_untracked(),
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
            let selected_provider = match &command {
                IntelligenceCommand::ProviderQuery { provider, .. } => Some(provider.clone()),
                IntelligenceCommand::Query(
                    Query::ResolveAction { .. } | Query::ReviewAction { .. },
                ) => result_provider.get_value(),
                _ => None,
            };
            let query = match command {
                IntelligenceCommand::Hover => Query::Hover { position },
                IntelligenceCommand::Completion | IntelligenceCommand::Suggest => {
                    Query::Completion { position }
                }
                IntelligenceCommand::HoverAt(_) => Query::Hover { position },
                IntelligenceCommand::InsertSuggestion(id) => Query::ResolveCompletion { id },
                IntelligenceCommand::Definition => Query::Definition { position },
                IntelligenceCommand::TypeDefinition => Query::TypeDefinition { position },
                IntelligenceCommand::Implementation => Query::Implementation { position },
                IntelligenceCommand::References => Query::References { position },
                IntelligenceCommand::Symbols => Query::Symbols,
                IntelligenceCommand::WorkspaceSymbols(query) => Query::WorkspaceSymbols { query },
                IntelligenceCommand::Rename(new_name) => Query::Rename { position, new_name },
                IntelligenceCommand::Formatting => Query::Formatting {
                    tab_width: u32::try_from(self.rules_untracked().indentation.tab_width())
                        .unwrap_or(4),
                    insert_spaces: self.rules_untracked().indentation.style == IndentStyle::Spaces,
                },
                IntelligenceCommand::RangeFormatting => {
                    let selected = captured.selection.range();
                    if selected.is_empty() {
                        error.set(Some("Select the code to format first.".into()));
                        return;
                    }
                    let range = match (
                        protocol::position(&captured.source, selected.start),
                        protocol::position(&captured.source, selected.end),
                    ) {
                        (Ok(start), Ok(end)) => protocol::Range { start, end },
                        _ => return,
                    };
                    Query::RangeFormatting {
                        range,
                        tab_width: u32::try_from(self.rules_untracked().indentation.tab_width())
                            .unwrap_or(4),
                        insert_spaces: self.rules_untracked().indentation.style
                            == IndentStyle::Spaces,
                    }
                }
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
                IntelligenceCommand::ProviderQuery { query, .. } => query,
            };
            let background = matches!(query, Query::Synchronize | Query::Saved | Query::Close);
            let action_followup = matches!(
                query,
                Query::ResolveAction { .. } | Query::ReviewAction { .. }
            );
            let workspace_discovery =
                matches!(query, Query::CodeActions { .. } | Query::Rename { .. });
            let retained_workspace = if action_followup {
                workspace_files.with_value(|saved| {
                    saved
                        .as_ref()
                        .filter(|(snapshot, _)| current(snapshot))
                        .map(|(_, files)| files.clone())
                })
            } else {
                None
            };
            if read_only.get_untracked()
                && matches!(
                    query,
                    Query::Completion { .. }
                        | Query::ResolveCompletion { .. }
                        | Query::Rename { .. }
                        | Query::Formatting { .. }
                        | Query::RangeFormatting { .. }
                        | Query::CodeActions { .. }
                        | Query::ResolveAction { .. }
                        | Query::ReviewAction { .. }
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
            if let Some(previous) = application_abort.get_value() {
                previous.abort();
            }
            application_abort.set_value(None);
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
                    if !action_followup {
                        workspace_files.set_value(None);
                    }
                    popup.set(None);
                    error.set(None);
                    result.set(None);
                    analysis.set_value(None);
                    clear_workspace_review(review, resources, review_epoch, resource_context);
                }
            }
            spawn_local(async move {
                let operation = async {
                    let prepared_files = if workspace_discovery {
                        if let Some(context) = resource_context {
                            // Resource capability failure does not revoke the
                            // ordinary validated text-only editor capability.
                            let files = context.prepare_files(&captured).await;
                            if ticket.try_get_value() != Some(request_ticket) || !current(&captured)
                            {
                                return Err(
                                    "Editor context changed during folder preparation".into()
                                );
                            }
                            workspace_files.set_value(Some((captured.clone(), files.clone())));
                            Some(files)
                        } else {
                            None
                        }
                    } else if action_followup {
                        if resource_context.is_some() && retained_workspace.is_none() {
                            return Err(
                                "Request fresh code actions before resolving this review".into()
                            );
                        }
                        retained_workspace
                    } else {
                        None
                    };
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
                    if !current(&captured) {
                        return Err("Editor context changed".into());
                    }
                    let open = source
                        .get_untracked()
                        .ok_or("The editor sources are unavailable")?
                        .2
                        .into_iter()
                        .map(|(path, source)| LanguageDocumentRequest {
                            document: openwebide_core::plugins::languages::source_digest(&path),
                            path,
                            source,
                            revision: captured.revision,
                            tab_width: self.rules_untracked().indentation.tab_width(),
                        })
                        .collect::<Vec<_>>();
                    let document = LanguageDocumentRequest {
                        document: openwebide_core::plugins::languages::source_digest(
                            &captured.presentation.path,
                        ),
                        path: captured.presentation.path.clone(),
                        revision: captured.revision,
                        source: captured.source.clone(),
                        tab_width: self.rules_untracked().indentation.tab_width(),
                    };
                    let history = state.intelligence_documents.get_value();
                    if matches!(query, Query::Synchronize | Query::Saved) {
                        return client_service
                            .diagnostics(
                                scope.project,
                                &client,
                                document,
                                &open,
                                &history,
                                matches!(query, Query::Saved),
                            )
                            .await
                            .map(IntelligenceReply::Diagnostics);
                    }
                    let selected = if let Some(namespace) = &selected_provider {
                        client_service.diagnostic_service(
                            namespace,
                            &captured.presentation.path,
                            &captured.source,
                        )?
                    } else {
                        client_service.for_query(&query)?
                    };
                    selected
                        .synchronize_intelligence(
                            scope.project,
                            &client,
                            &document,
                            &open,
                            &history,
                        )
                        .await?;
                    if !current(&captured) {
                        return Err("Editor context changed".into());
                    }
                    let provider = selected.provider().clone();
                    let response = selected
                        .intelligence(&client, document.clone(), query.clone())
                        .await?;
                    let analysis = AnalysisResult::capture(
                        &captured,
                        &query,
                        client,
                        document,
                        selected,
                        &response,
                        prepared_files,
                    )?;
                    Ok(IntelligenceReply::Server(Box::new((
                        response, provider, analysis,
                    ))))
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
                    Ok(Ok(IntelligenceReply::Diagnostics(reports))) => {
                        publish_diagnostics.run((captured, reports, true));
                    }
                    Ok(Ok(IntelligenceReply::Server(response))) => {
                        let (response, provider, captured_analysis) = *response;
                        publish_diagnostics.run((
                            captured.clone(),
                            vec![ProviderDiagnostics {
                                provider: provider.clone(),
                                diagnostics: response.diagnostics.clone(),
                                ready: response.diagnostics_ready,
                                server: Some(response.server.clone()),
                                error: None,
                            }],
                            false,
                        ));
                        if !background {
                            if self.current_selection().unwrap_or_default() != captured.selection {
                                return;
                            }
                            result_provider.set_value(selected_provider.clone());
                            analysis.set_value(captured_analysis);
                            if mode == Presentation::Insert {
                                snapshot.set_value(Some(captured));
                                match insert.run(response.result) {
                                    Ok(()) => {}
                                    Err(message) => {
                                        error.set(None);
                                        popup_error.set(Some(message));
                                    }
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
        Effect::new(move |_| {
            if let Some(followup) = completion_followup.get() {
                completion_followup.set(None);
                spawn_local(async move {
                    crate::util::sleep_ms(0).await;
                    if current(&followup) && self.current_selection() == Some(followup.selection) {
                        request.run(IntelligenceCommand::Suggest);
                    }
                });
            }
        });
        let workspace_actions = use_context::<crate::state_actions::workspace::WorkspaceActions>();
        // This callback belongs to the editor's owner. Retaining that owner
        // here would keep its timers, listeners and source buffers alive after
        // unmount; upgrade only while constructing a live navigation completion.
        let navigation_owner = Owner::current()?.downgrade();
        let navigation_completion = StoredValue::new(None::<Callback<Result<(), String>>>);
        let navigate = Callback::new(move |target: results::Target| {
            let Some(navigation_owner) = navigation_owner.upgrade() else {
                return;
            };
            if let Err(message) = target.validate() {
                error.set(Some(message));
                return;
            }
            let Some(captured) = snapshot.get_value().filter(current) else {
                return;
            };
            if let Some(previous) = navigation_completion.get_value() {
                previous.dispose();
            }
            navigation_completion.set_value(None);
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
            let destination = path.clone();
            // The result dialog closes before the asynchronous file open
            // finishes. Keep its completion under the editor's owner.
            let completion = navigation_owner.with(|| {
                Callback::new(move |opened: Result<(), String>| {
                    if self.capacity.try_get_untracked().is_none()
                        || self.account_generation() != captured.presentation.account
                        || !self.is_current(captured.presentation.project, &path)
                        || repository.try_get_untracked() != Some(captured.service.repository)
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
                                    source_revision.get_untracked(),
                                    range.start,
                                )));
                            }
                        }
                        Err(message) => error.set(Some(message)),
                    }
                })
            });
            // Retain one completion only; a later jump revokes the older
            // completion and releases its captured source.
            navigation_completion.set_value(Some(completion));
            actions.open_with_completion.run((destination, completion));
        });
        let previous_context = StoredValue::new(None);
        let refresh_epoch = StoredValue::new(0_u64);
        Effect::new(move |_| {
            identity.with(|_| ());
            source_revision.with(|_| ());
            composition.with(|_| ());
            let dirty = dirty.get();
            let observed = (
                identity.get_untracked(),
                source_revision.get_untracked(),
                composition.get_untracked(),
                dirty,
                presentation.get_untracked(),
            );
            let changed =
                previous_context.with_value(|previous| previous.as_ref() != Some(&observed));
            if !changed {
                return;
            }
            // Coalesced refresh notifications may revisit an unchanged context.
            // Only effective changes revoke in-flight work and visible reviews.
            previous_context.set_value(Some(observed));
            refresh_epoch.update_value(|epoch| *epoch = epoch.wrapping_add(1));
            let key = presentation.with(|scope| {
                scope
                    .as_ref()
                    .map(|scope| (scope.project, scope.path.clone()))
            });
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
            provider_diagnostics.set(Vec::new());
            result_provider.set_value(None);
            diagnostics_source.set_value(None);
            snapshot.set_value(None);
            analysis.set_value(None);
            workspace_files.set_value(None);
            if let Some(previous) = application_abort.get_value() {
                previous.abort();
            }
            application_abort.set_value(None);
            clear_workspace_review(review, resources, review_epoch, resource_context);
            // Popup dismissal invalidates its presentation ticket, but the
            // current source still needs diagnostics. Only a newer editor
            // context may supersede this refresh.
            let expected = refresh_epoch.get_value();
            if !available.get_untracked() {
                service.set_value(None);
                return;
            }
            spawn_local(async move {
                crate::util::sleep_ms(400).await;
                if refresh_epoch.try_get_value() == Some(expected) {
                    request.run(IntelligenceCommand::Query(if saved {
                        Query::Saved
                    } else {
                        Query::Synchronize
                    }));
                }
            });
        });
        on_cleanup(move || {
            workspace_files.try_update_value(|files| *files = None);
            if let Some(Some(application)) = application_abort.try_get_value() {
                application.abort();
            }
            if let Some(Some(automatic)) = automatic_abort.try_get_value() {
                automatic.abort();
            }
            if let Some(Some(pending)) = pending.try_get_value() {
                pending.abort();
            }
        });
        Effect::new(move |_| {
            if !busy.get()
                && !popup_busy.get()
                && result.with(Option::is_none)
                && popup.with(Option::is_none)
                && review.with(Option::is_none)
                && resources.with(Option::is_none)
            {
                workspace_files.set_value(None);
                analysis.set_value(None);
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
            primary_available,
            identity,
            context_revision,
            source_revision,
            request_presentation,
            queued,
            review,
            resources,
            review_epoch,
            resource_context,
            busy,
            rename_open,
            error,
            diagnostics,
            provider_diagnostics,
            diagnostics_source,
            result,
            popup,
            popup_busy,
            popup_error,
            dismiss_popup,
            request,
            dismiss,
            apply_text,
            insert,
            navigate,
            editor: self,
            navigation,
            workspace_actions,
            snapshot,
            analysis,
            application_abort,
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
            && self.source_revision.get_untracked() == revision)
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
            self.navigation.set(Some((
                scope,
                self.source_revision.get_untracked(),
                offset.start,
            )));
        }
    }
    pub fn navigate_next_diagnostic(self, backwards: bool) -> bool {
        if !self.diagnostics_current() {
            return false;
        }
        let Some(selection) = self.editor.current_selection() else {
            return false;
        };
        let source = self.editor.source();
        let Ok(position) = protocol::position(&source, selection.head) else {
            return false;
        };
        let mut ranges = self.diagnostics.with_untracked(|diagnostics| {
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.range)
                .collect::<Vec<_>>()
        });
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges.dedup();
        let selected = if backwards {
            ranges
                .iter()
                .rev()
                .find(|range| range.start < position)
                .or_else(|| ranges.last())
        } else {
            ranges
                .iter()
                .find(|range| range.start > position)
                .or_else(|| ranges.first())
        };
        if let Some(range) = selected {
            self.navigate_diagnostic(*range);
            true
        } else {
            false
        }
    }
    pub fn diagnostics_current(self) -> bool {
        self.diagnostics_source
            .try_get_value()
            .flatten()
            .is_some_and(|(scope, revision)| {
                self.editor.presentation_scope().as_ref() == Some(&scope)
                    && self.source_revision.try_get_untracked() == Some(revision)
            })
    }
    fn snapshot_current(self, captured: &Snapshot) -> bool {
        untrack(|| self.editor.presentation_scope()).as_ref() == Some(&captured.presentation)
            && self.source_revision.try_get_untracked() == Some(captured.revision)
            && self.identity.try_get_untracked().flatten().as_ref() == Some(&captured.service)
    }

    pub fn apply_completion(self, item: &serde_json::Value) {
        if let Err(message) = self.insert.run(item.clone()) {
            self.error.set(Some(message));
        }
    }
}

fn prepare_completion(
    snapshot: &Snapshot,
    item: &serde_json::Value,
) -> Result<results::Completion, String> {
    let position = protocol::position(&snapshot.source, snapshot.selection.head)?;
    let path = std::path::Path::new(&snapshot.presentation.path);
    let variables = std::collections::BTreeMap::from([
        (
            "TM_FILENAME".into(),
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "TM_FILENAME_BASE".into(),
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "RELATIVE_FILEPATH".into(),
            snapshot.presentation.path.clone(),
        ),
        (
            "TM_SELECTED_TEXT".into(),
            snapshot
                .source
                .get(snapshot.selection.range())
                .unwrap_or_default()
                .into(),
        ),
        (
            "TM_CURRENT_LINE".into(),
            snapshot.source[..snapshot.selection.head]
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .to_owned()
                + snapshot.source[snapshot.selection.head..]
                    .split('\n')
                    .next()
                    .unwrap_or_default(),
        ),
        ("TM_LINE_INDEX".into(), position.line.to_string()),
        ("TM_LINE_NUMBER".into(), (position.line + 1).to_string()),
    ]);
    results::completion(&snapshot.source, position, item, &variables)
}
