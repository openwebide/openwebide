//! Suggest only owned context; explicit selection attaches it to the user's draft.
use crate::{
    backend::Api,
    state::{
        auth::AuthState, chat::ChatState, memories::MemoriesState, projects::ProjectsState,
        settings::SettingsState, workspace::WorkspaceState,
    },
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{AssistanceKind, AssistanceRequest, ConversationEntry, ProjectMemory, Role};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCandidate {
    File(String),
    Memory(ProjectMemory),
    Session(i64, String),
}
impl ContextCandidate {
    pub fn id(&self) -> String {
        match self {
            Self::File(path) => format!("file:{path}"),
            Self::Memory(entry) => format!("memory:{}", entry.id),
            Self::Session(id, _) => format!("session:{id}"),
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::File(path) => format!("File: {path}"),
            Self::Memory(entry) => format!("Memory: {}", entry.title),
            Self::Session(_, name) => format!("Chat: {name}"),
        }
    }
}
#[derive(Clone, Copy)]
pub struct ContextAssistance {
    pub suggestions: RwSignal<Vec<ContextCandidate>>,
    pub error: RwSignal<Option<String>>,
    pub attach: Callback<ContextCandidate>,
}
impl ContextAssistance {
    pub fn from_context() -> Self {
        let api = expect_context::<Api>();
        let auth = expect_context::<AuthState>();
        let chat = expect_context::<ChatState>();
        let projects = expect_context::<ProjectsState>();
        let workspace = expect_context::<WorkspaceState>();
        let memories = expect_context::<MemoriesState>();
        let settings = expect_context::<SettingsState>();
        let suggestions = RwSignal::new(Vec::new());
        let error = RwSignal::new(None);
        let revision = StoredValue::new(0u64);
        let queue = StoredValue::new_local(super::assistance::AssistanceQueue::from_context());
        Effect::new(move |_| {
            let epoch = auth.generation.get();
            let project_id = projects.active_project.get();
            let session_id = chat.active_session.get();
            let draft = chat.draft.get();
            let streaming = chat.streaming.get();
            workspace.entries.track();
            memories.data.track();
            chat.sessions.track();
            revision.update_value(|revision| *revision += 1);
            let ticket = revision.get_value();
            suggestions.set(Vec::new());
            error.set(None);
            if streaming || draft.trim().chars().count() < 12 {
                return;
            }
            let connection_id = chat
                .sessions
                .with_untracked(|sessions| {
                    sessions
                        .iter()
                        .find(|session| Some(session.id) == session_id)
                        .and_then(|session| session.connection_id)
                })
                .or(settings.default_connection.get_untracked())
                .or_else(|| {
                    settings.model_setup.with_untracked(|setup| {
                        setup.defaults.primary.as_ref().map(|model| model.server_id)
                    })
                });
            let Some(connection_id) = connection_id else {
                return;
            };
            let mut candidates = Vec::new();
            if project_id.is_some() {
                let paths = workspace.entries.with_untracked(|entries| {
                    entries
                        .values()
                        .flatten()
                        .filter(|entry| !entry.is_dir)
                        .map(|entry| entry.path.clone())
                        .collect::<std::collections::BTreeSet<_>>()
                });
                candidates.extend(paths.into_iter().take(60).map(ContextCandidate::File));
                if let Some(data) = memories.data.get_untracked()
                    && data.enabled
                {
                    candidates.extend(
                        data.entries
                            .into_iter()
                            .take(20)
                            .map(ContextCandidate::Memory),
                    );
                }
            }
            candidates.extend(chat.sessions.with_untracked(|sessions| {
                sessions
                    .iter()
                    .filter(|session| {
                        session.project_id == project_id
                            && Some(session.id) != session_id
                            && !session.archived
                    })
                    .take(20)
                    .map(|session| ContextCandidate::Session(session.id, session.name.clone()))
                    .collect::<Vec<_>>()
            }));
            candidates.retain(|candidate| match candidate {
                ContextCandidate::File(path) => {
                    !draft.contains(&openwebide_core::prompt::mention_token(
                        openwebide_core::prompt::MentionKind::File,
                        path,
                    ))
                }
                ContextCandidate::Memory(entry) => {
                    !draft.contains(&format!("(memory #{})", entry.id))
                }
                ContextCandidate::Session(id, _) => !draft.contains(&format!("(chat #{id})")),
            });
            if candidates.is_empty() {
                return;
            }
            let input = serde_json::json!({
                "draft": draft.chars().take(2000).collect::<String>(),
                "candidates": candidates.iter().map(|candidate| {
                    serde_json::json!({"id": candidate.id(), "label": candidate.label()})
                }).collect::<Vec<_>>()
            })
            .to_string();
            spawn_local(async move {
                crate::util::sleep_ms(1500).await;
                let current = move || {
                    auth.generation.try_get_untracked() == Some(epoch)
                        && projects.active_project.try_get_untracked() == Some(project_id)
                        && chat.active_session.try_get_untracked() == Some(session_id)
                        && revision.try_get_value() == Some(ticket)
                        && chat.streaming.try_get_untracked() == Some(false)
                };
                if !current() {
                    return;
                }
                let Some(queue) = queue.try_with_value(Clone::clone) else {
                    return;
                };
                let _permit = queue.permit().await;
                if !current() {
                    return;
                }
                let request = AssistanceRequest {
                    kind: AssistanceKind::Context,
                    model: None,
                    staged_draft: false,
                    connection_id,
                    session_id,
                    project_id,
                    input,
                };
                if let Ok(Some(text)) = api.with_value(Clone::clone).assistance(&request).await
                    && current()
                {
                    // Resolve against the exact candidates supplied, never model-invented paths/IDs.
                    let mut seen = std::collections::HashSet::new();
                    suggestions.set(
                        text.lines()
                            .filter(|id| seen.insert(id.trim().to_owned()))
                            .filter_map(|id| {
                                candidates
                                    .iter()
                                    .find(|candidate| candidate.id() == id.trim())
                                    .cloned()
                            })
                            .take(3)
                            .collect(),
                    );
                }
            });
        });
        let attach = Callback::new(move |candidate: ContextCandidate| {
            if !suggestions.with_untracked(|items| items.contains(&candidate))
                || chat.streaming.get_untracked()
            {
                return;
            }
            let epoch = auth.generation.get_untracked();
            let project = projects.active_project.get_untracked();
            let session = chat.active_session.get_untracked();
            let original = chat.draft.get_untracked();
            let ticket = revision.get_value();
            spawn_local(async move {
                let current = move || {
                    auth.generation.try_get_untracked() == Some(epoch)
                        && projects.active_project.try_get_untracked() == Some(project)
                        && chat.active_session.try_get_untracked() == Some(session)
                        && revision.try_get_value() == Some(ticket)
                        && chat.streaming.try_get_untracked() == Some(false)
                };
                let result: Result<String, String> = async {
                    match candidate {
                        ContextCandidate::File(path) => {
                            if !workspace.entries.with_untracked(|entries| {
                                entries
                                    .values()
                                    .flatten()
                                    .any(|entry| entry.path == path && !entry.is_dir)
                            }) {
                                return Err("File suggestions changed. Try again.".into());
                            }
                            Ok(openwebide_core::prompt::mention_token(
                                openwebide_core::prompt::MentionKind::File,
                                &path,
                            ))
                        }
                        ContextCandidate::Memory(entry) => {
                            if !memories.data.with_untracked(|data| {
                                data.as_ref().is_some_and(|data| {
                                    data.enabled && data.entries.contains(&entry)
                                })
                            }) {
                                return Err("This memory changed. Refresh suggestions.".into());
                            }
                            Ok(format!(
                                "Stored reference — {} (memory #{}):\n{}",
                                entry.title, entry.id, entry.content
                            ))
                        }
                        ContextCandidate::Session(id, name) => {
                            let entries = api.with_value(Clone::clone).list_messages(id).await?;
                            let text = entries
                                .into_iter()
                                .rev()
                                .filter_map(|entry| match entry {
                                    ConversationEntry::Message(message)
                                        if matches!(message.role, Role::User | Role::Assistant) =>
                                    {
                                        Some(
                                            format!(
                                                "{:?}: {}",
                                                message.role,
                                                if message.role == Role::User {
                                                    openwebide_core::PromptContent::decode(
                                                        &message.content,
                                                    )
                                                    .summary_text()
                                                } else {
                                                    openwebide_core::strip_reasoning(
                                                        &message.content,
                                                    )
                                                    .to_owned()
                                                }
                                            )
                                            .chars()
                                            .take(1000)
                                            .collect::<String>(),
                                        )
                                    }
                                    _ => None,
                                })
                                .take(4)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect::<Vec<_>>()
                                .join("\n");
                            Ok(format!(
                                "Previous conversation reference — {name} (chat #{id}):\n{text}"
                            ))
                        }
                    }
                }
                .await;
                if !current() {
                    return;
                }
                match result {
                    Ok(text) => chat.draft.set(format!("{original}\n\n{text}")),
                    Err(message) => error.set(Some(message)),
                }
            });
        });
        Self {
            suggestions,
            error,
            attach,
        }
    }
}
