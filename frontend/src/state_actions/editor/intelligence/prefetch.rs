//! Optional server warming. Speculative payloads never enter editor state or history.
use super::*;
use openwebide_core::plugins::languages::intelligence::PrefetchKind;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Weak,
};

const IDLE_MS: i32 = 750;
// The native RPC has a two-second budget; also bound authority/transport waits.
const REQUEST_MS: i32 = 3000;
const MAX_FILES: usize = 4;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_SOURCE_BYTES: usize = 512 * 1024;
const MAX_RECENT: usize = 16;

type Sources = (
    EditorPresentationScope,
    Arc<String>,
    BTreeMap<String, Arc<String>>,
);

#[derive(Clone)]
pub(super) struct Ready {
    pub scope: ServiceScope,
    pub service: ProjectLanguageService,
    pub client: String,
    pub revision: u64,
    pub server: String,
}

struct Candidate {
    provider: RegisteredLanguage,
    document: LanguageDocumentRequest,
    close: bool,
    retained: bool,
}

#[derive(Default)]
struct PrefetchHistory {
    context: Option<(ServiceScope, String, u64)>,
    previous: BTreeMap<String, Weak<String>>,
    recent: VecDeque<String>,
    related: VecDeque<String>,
    attempted: Vec<(String, PrefetchKind)>,
    read: Vec<String>,
}

impl PrefetchHistory {
    fn observe(&mut self, ready: &Ready) {
        let context = (ready.scope.clone(), ready.server.clone(), ready.revision);
        if self
            .context
            .as_ref()
            .is_some_and(|old| old.0 != context.0 || old.1 != context.1)
        {
            self.previous.clear();
            self.recent.clear();
            self.related.clear();
        }
        if self.context.as_ref() != Some(&context) {
            self.attempted.clear();
            self.read.clear();
            // Related paths are bounded scheduling hints, never reusable results.
            // Keep them through edits; candidates still use the current sources.
            self.context = Some(context);
        }
    }

    fn candidates(&mut self, ready: &Ready, sources: &Sources, tab_width: usize) -> Vec<Candidate> {
        self.observe(ready);
        let mut open = sources.2.clone();
        open.insert(sources.0.path.clone(), sources.1.clone());
        for (path, source) in &open {
            let changed = self
                .previous
                .get(path)
                .and_then(Weak::upgrade)
                .is_none_or(|previous| !Arc::ptr_eq(&previous, source));
            if changed {
                self.recent.retain(|old| old != path);
                self.recent.push_front(path.clone());
            }
        }
        self.recent.truncate(MAX_RECENT);
        self.previous = open
            .iter()
            .map(|(path, source)| (path.clone(), Arc::downgrade(source)))
            .collect();
        let order = std::iter::once(&sources.0.path)
            .chain(self.related.iter())
            .chain(self.recent.iter())
            .chain(open.keys());
        let mut selected = Vec::new();
        let mut bytes = 0;
        for path in order {
            if selected.len() == MAX_FILES {
                break;
            }
            if selected
                .iter()
                .any(|candidate: &Candidate| candidate.document.path == *path)
            {
                continue;
            }
            let Some(source) = open.get(path) else {
                continue;
            };
            if !protocol::relative_path(path)
                || source.len() > MAX_FILE_BYTES
                || source.len() > MAX_SOURCE_BYTES - bytes
            {
                continue;
            }
            let Ok(Some(provider)) = ready.scope.registry.server_document(
                &ready.scope.provider,
                path,
                source,
                &ready.scope.preferences,
            ) else {
                continue;
            };
            bytes += source.len();
            selected.push(Candidate {
                provider: provider.clone(),
                document: LanguageDocumentRequest {
                    document: openwebide_core::plugins::languages::source_digest(path),
                    path: path.clone(),
                    source: source.clone(),
                    revision: ready.revision,
                    tab_width,
                },
                close: false,
                retained: false,
            });
        }
        selected
    }

    fn closed_paths(&self, sources: &Sources) -> Vec<String> {
        let mut paths = Vec::new();
        for path in self.related.iter().chain(self.recent.iter()) {
            if path == &sources.0.path
                || sources.2.contains_key(path)
                || !protocol::relative_path(path)
                || paths.contains(path)
            {
                continue;
            }
            paths.push(path.clone());
            if paths.len() == MAX_FILES {
                break;
            }
        }
        paths
    }

    fn remember_related(&mut self, targets: &[results::Target]) {
        for target in targets.iter().take(MAX_RECENT) {
            if target.validate().is_err() {
                continue;
            }
            let Some(path) = &target.path else {
                continue;
            };
            self.related.retain(|old| old != path);
            self.related.push_front(path.clone());
        }
        self.related.truncate(MAX_RECENT);
    }
}

/// Closed tabs can retain unsaved documents. Their current text always wins
/// over a filesystem read; clean retained snapshots are not filesystem caches.
fn retained_source(editor: EditorActions, project: i64, path: &str) -> Option<Arc<String>> {
    let key = (project, path.to_string());
    editor
        .workspace
        .editor_buffers
        .with_untracked(|buffers| {
            buffers
                .get(&key)
                .filter(|buffer| buffer.dirty)
                .map(|buffer| buffer.content.shared())
        })
        .or_else(|| {
            editor
                .workspace
                .editor_documents
                .with_untracked(|documents| {
                    documents
                        .get(&key)
                        .filter(|document| document.is_dirty())
                        .map(openwebide_core::editor::Document::shared_text)
                })
        })
}

#[derive(Clone, Copy)]
pub(super) struct Prefetch {
    ready: StoredValue<Option<Ready>, LocalStorage>,
    history: StoredValue<PrefetchHistory, LocalStorage>,
    pending: StoredValue<Option<AbortHandle>>,
    changed: RwSignal<u64>,
    ticket: StoredValue<u64>,
}

impl Prefetch {
    pub fn new() -> Self {
        Self {
            ready: StoredValue::new_local(None),
            history: StoredValue::new_local(PrefetchHistory::default()),
            pending: StoredValue::new(None),
            changed: RwSignal::new(0),
            ticket: StoredValue::new(0),
        }
    }

    fn cancel(self) {
        if let Some(Some(pending)) = self.pending.try_get_value() {
            pending.abort();
        }
        self.pending.try_update_value(|pending| *pending = None);
        self.ticket
            .try_update_value(|ticket| *ticket = ticket.wrapping_add(1));
    }

    /// Foreground work owns admission, including automatic diagnostics/completion.
    pub fn interrupt(self) {
        self.cancel();
        self.ready.try_update_value(|ready| *ready = None);
    }

    /// Called only after a current interactive/synchronization response. This
    /// never prepares a provider, issues a grant, or starts a process itself.
    pub fn ready(self, ready: Ready) {
        self.history.update_value(|history| history.observe(&ready));
        self.ready.set_value(Some(ready));
        self.changed
            .update(|changed| *changed = changed.wrapping_add(1));
    }

    pub fn satisfied(self, path: &str, kind: PrefetchKind) {
        self.history.update_value(|history| {
            let entry = (path.to_string(), kind);
            if !history.attempted.contains(&entry) && history.attempted.len() < MAX_FILES * 2 {
                history.attempted.push(entry);
            }
        });
    }

    pub fn related(self, query: &Query, response: &Response) {
        if matches!(
            query,
            Query::Definition { .. }
                | Query::TypeDefinition { .. }
                | Query::Implementation { .. }
                | Query::References { .. }
        ) && let Some(items) = response.result.as_array()
        {
            let targets = items
                .iter()
                .take(MAX_RECENT)
                .filter_map(|item| serde_json::from_value::<results::Target>(item.clone()).ok())
                .collect::<Vec<_>>();
            self.history
                .update_value(|history| history.remember_related(&targets));
        }
    }

    pub fn install(
        self,
        editor: EditorActions,
        identity: Memo<Option<ServiceScope>>,
        revision: Memo<u64>,
        sources: Memo<Option<Sources>>,
        allowed: Memo<bool>,
    ) {
        let api = use_context::<Api>();
        let projects = use_context::<crate::state::projects::ProjectsState>();
        Effect::new(move |_| {
            self.changed.track();
            identity.track();
            revision.track();
            let can_prefetch = allowed.get();
            self.cancel();
            let Some(ready) = self.ready.get_value().filter(|ready| {
                can_prefetch
                    && identity.get_untracked().as_ref() == Some(&ready.scope)
                    && revision.get_untracked() == ready.revision
            }) else {
                return;
            };
            let expected = self.ticket.get_value();
            let (abort, registration) = AbortHandle::new_pair();
            self.pending.set_value(Some(abort));
            let expected_scope = ready.scope.clone();
            let expected_revision = ready.revision;
            let current = move || {
                self.ticket.try_get_value() == Some(expected)
                    && identity.try_get_untracked().flatten().as_ref() == Some(&expected_scope)
                    && revision.try_get_untracked() == Some(expected_revision)
                    && allowed.try_get_untracked() == Some(true)
            };
            spawn_local(async move {
                let operation = async {
                    crate::util::sleep_ms(IDLE_MS).await;
                    if !current() {
                        return;
                    }
                    let Some(sources) = sources.try_get_untracked().flatten() else {
                        return;
                    };
                    if self
                        .history
                        .with_value(|history| history.attempted.len() >= MAX_FILES * 2)
                    {
                        return;
                    }
                    let mut candidates = self
                        .history
                        .try_update_value(|history| {
                            history.candidates(
                                &ready,
                                &sources,
                                editor.rules_untracked().indentation.tab_width(),
                            )
                        })
                        .unwrap_or_default();
                    let closed = self
                        .history
                        .with_value(|history| history.closed_paths(&sources));
                    let mut bytes = candidates
                        .iter()
                        .map(|candidate| candidate.document.source.len())
                        .sum::<usize>();
                    let mut files = candidates.len();
                    for path in closed {
                        if !current() || files == MAX_FILES || bytes == MAX_SOURCE_BYTES {
                            break;
                        }
                        if self.history.with_value(|history| {
                            [PrefetchKind::Symbols, PrefetchKind::Diagnostics]
                                .iter()
                                .all(|kind| history.attempted.contains(&(path.clone(), *kind)))
                        }) {
                            continue;
                        }
                        // A failed read or provider rejection still consumes a
                        // file admission; it cannot extend the batch with more
                        // filesystem work than successful analysis would use.
                        files += 1;
                        let max = MAX_FILE_BYTES.min(MAX_SOURCE_BYTES - bytes);
                        let retained = retained_source(editor, ready.scope.project, &path);
                        let is_retained = retained.is_some();
                        let source = if let Some(source) = retained {
                            if source.len() > max {
                                continue;
                            }
                            bytes += source.len();
                            source
                        } else {
                            if self.history.with_value(|history| {
                                history.read.len() >= MAX_FILES || history.read.contains(&path)
                            }) {
                                continue;
                            }
                            let Some(workspace) = api.zip(projects).and_then(|(api, projects)| {
                                crate::workspace::Workspace::for_project(
                                    api,
                                    projects,
                                    ready.scope.project,
                                )
                            }) else {
                                break;
                            };
                            self.history
                                .update_value(|history| history.read.push(path.clone()));
                            // Reserve the complete read budget even if decoding,
                            // permissions or provider matching later decline it.
                            bytes += max;
                            let read = Box::pin(workspace.read_text_with_limit(&path, max));
                            let deadline = Box::pin(crate::util::sleep_ms(REQUEST_MS));
                            let result = match futures::future::select(read, deadline).await {
                                futures::future::Either::Left((result, _)) => result.ok(),
                                futures::future::Either::Right(_) => None,
                            };
                            if !current() {
                                return;
                            }
                            let Some(source) = result else {
                                // Failed optional reads are bounded attempts too.
                                self.satisfied(&path, PrefetchKind::Symbols);
                                self.satisfied(&path, PrefetchKind::Diagnostics);
                                continue;
                            };
                            Arc::new(source)
                        };
                        if source.len() > max || source.contains('\0') {
                            continue;
                        }
                        let Ok(Some(provider)) = ready.scope.registry.server_document(
                            &ready.scope.provider,
                            &path,
                            &source,
                            &ready.scope.preferences,
                        ) else {
                            continue;
                        };
                        candidates.push(Candidate {
                            provider: provider.clone(),
                            document: LanguageDocumentRequest {
                                document: openwebide_core::plugins::languages::source_digest(&path),
                                path,
                                source,
                                revision: ready.revision,
                                tab_width: editor.rules_untracked().indentation.tab_width(),
                            },
                            close: true,
                            retained: is_retained,
                        });
                    }
                    for candidate in candidates {
                        let kinds = if ready
                            .scope
                            .provider
                            .definition
                            .features
                            .language_server
                            .as_ref()
                            .is_some_and(|server| server.role.is_language())
                        {
                            &[PrefetchKind::Symbols, PrefetchKind::Diagnostics][..]
                        } else {
                            &[PrefetchKind::Diagnostics][..]
                        };
                        for &kind in kinds {
                            if !current()
                                || self
                                    .history
                                    .with_value(|history| history.attempted.len() >= MAX_FILES * 2)
                            {
                                return;
                            }
                            let path = candidate.document.path.clone();
                            if candidate.close {
                                let retained = retained_source(editor, ready.scope.project, &path);
                                if match retained {
                                    Some(source) => {
                                        !candidate.retained
                                            || !Arc::ptr_eq(&source, &candidate.document.source)
                                    }
                                    None => candidate.retained,
                                } {
                                    continue;
                                }
                            }
                            if self.history.with_value(|history| {
                                history.attempted.contains(&(path.clone(), kind))
                            }) {
                                continue;
                            }
                            let Ok(service) = ready
                                .service
                                .for_server_language(candidate.provider.clone())
                            else {
                                continue;
                            };
                            // Native admission is the only warm-service/priority
                            // authority. Never call ordinary synchronization here.
                            let query = Box::pin(service.intelligence(
                                &ready.client,
                                candidate.document.clone(),
                                Query::Prefetch {
                                    kind,
                                    close: candidate.close,
                                },
                            ));
                            let deadline = Box::pin(crate::util::sleep_ms(REQUEST_MS));
                            let response = match futures::future::select(query, deadline).await {
                                futures::future::Either::Left((response, _)) => response,
                                futures::future::Either::Right(_) => {
                                    Err("Optional language prefetch exceeded its client budget"
                                        .into())
                                }
                            };
                            if !current() {
                                return;
                            }
                            self.history
                                .update_value(|history| history.attempted.push((path, kind)));
                            if response
                                .as_ref()
                                .is_ok_and(|response| response.server != ready.server)
                            {
                                return;
                            }
                            if response
                                .as_ref()
                                .is_ok_and(|response| response.diagnostics_ready)
                            {
                                self.satisfied(&candidate.document.path, PrefetchKind::Diagnostics);
                            }
                            crate::util::yield_task().await;
                        }
                    }
                };
                let _ = Abortable::new(operation, registration).await;
                if self.ticket.try_get_value() == Some(expected) {
                    self.pending.try_update_value(|pending| *pending = None);
                }
            });
        });
        on_cleanup(move || self.cancel());
    }
}
