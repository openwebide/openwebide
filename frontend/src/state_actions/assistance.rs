//! Shared, idle-only assistance workflows for local, remote and projectless chat.
use crate::{
    backend::Api,
    conversation::ConversationItem,
    state::{auth::AuthState, chat::ChatState, projects::ProjectsState},
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{AssistanceKind, AssistanceRequest, Role};

/// One queue for optional model work, shared by titles and chat assistance.
#[derive(Clone, Default)]
pub struct AssistanceQueue(std::sync::Arc<futures::lock::Mutex<()>>);
impl AssistanceQueue {
    pub fn from_context() -> Self {
        if let Some(queue) = use_context::<Self>() {
            return queue;
        }
        let queue = Self::default();
        provide_context(queue.clone());
        queue
    }
    pub async fn permit(&self) -> futures::lock::MutexGuard<'_, ()> {
        self.0.lock().await
    }
}

#[derive(Clone, Copy)]
pub struct ChatAssistance {
    pub recap: RwSignal<Option<String>>,
    pub next_actions: RwSignal<Vec<String>>,
    pub completion: RwSignal<Option<String>>,
}
impl ChatAssistance {
    pub fn new(api: Api, auth: AuthState, chat: ChatState, projects: ProjectsState) -> Self {
        let queue = StoredValue::new_local(AssistanceQueue::from_context());
        let settings = use_context::<crate::state::settings::SettingsState>();
        let result = Self {
            recap: RwSignal::new(None),
            next_actions: RwSignal::new(Vec::new()),
            completion: RwSignal::new(None),
        };
        let revision = StoredValue::new(0u64);
        Effect::new(move |_| {
            let epoch = auth.generation.get();
            let session_id = chat.active_session.get();
            let project_id = projects.active_project.get();
            let streaming = chat.streaming.get();
            let failure = chat.error.get();
            chat.messages.changed.track();
            revision.update_value(|revision| *revision += 1);
            let request_revision = revision.get_value();
            result.recap.set(None);
            result.next_actions.set(Vec::new());
            result.completion.set(None);
            if streaming || session_id.is_none() {
                return;
            }
            let Some(connection_id) = chat
                .sessions
                .with_untracked(|sessions| {
                    sessions
                        .iter()
                        .find(|session| {
                            Some(session.id) == session_id && session.project_id == project_id
                        })
                        .and_then(|session| session.connection_id)
                })
                .or_else(|| {
                    settings.and_then(|settings| {
                        settings.default_connection.get_untracked().or_else(|| {
                            settings.model_setup.with_untracked(|setup| {
                                setup.defaults.primary.as_ref().map(|model| model.server_id)
                            })
                        })
                    })
                })
            else {
                return;
            };
            let items = chat.messages.get_untracked();
            if !items
                .iter()
                .rev()
                .find_map(|item| match item {
                    ConversationItem::Message(message)
                        if matches!(message.role, Role::User | Role::Assistant) =>
                    {
                        Some(
                            message.role == Role::Assistant
                                && !openwebide_core::strip_reasoning(&message.content)
                                    .trim()
                                    .is_empty(),
                        )
                    }
                    _ => None,
                })
                .unwrap_or(false)
            {
                return;
            }
            let mut input = activity(&items);
            if let Some(failure) = failure {
                input.push_str(&format!("\nExecution error: {failure}"));
            }
            spawn_local(async move {
                // Foreground work wins. Debounce completed turns and navigation.
                crate::util::sleep_ms(2000).await;
                let current = move || {
                    auth.generation.try_get_untracked() == Some(epoch)
                        && chat.active_session.try_get_untracked() == Some(session_id)
                        && projects.active_project.try_get_untracked() == Some(project_id)
                        && chat.streaming.try_get_untracked() == Some(false)
                        && revision.try_get_value() == Some(request_revision)
                };
                for kind in [
                    AssistanceKind::Recap,
                    AssistanceKind::NextActions,
                    AssistanceKind::Completion,
                ] {
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
                        kind,
                        model: None,
                        staged_draft: false,
                        connection_id,
                        session_id,
                        project_id,
                        input: input.clone(),
                    };
                    let response = api.with_value(Clone::clone).assistance(&request).await;
                    if !current() {
                        return;
                    }
                    if let Ok(Some(text)) = response {
                        if kind == AssistanceKind::Recap {
                            result.recap.set(Some(text));
                        } else if kind == AssistanceKind::Completion {
                            result.completion.set(Some(text));
                        } else {
                            let mut prompts = Vec::new();
                            for line in text.lines().map(str::trim) {
                                if !line.is_empty()
                                    && line != "NONE"
                                    && !prompts.iter().any(|prompt| prompt == line)
                                {
                                    prompts.push(line.to_owned());
                                    if prompts.len() == 3 {
                                        break;
                                    }
                                }
                            }
                            result.next_actions.set(prompts);
                        }
                    }
                }
            });
        });
        result
    }
}

/// Include actual tool outcomes alongside messages so recaps cannot assume success.
fn activity(items: &[ConversationItem]) -> String {
    items
        .iter()
        .rev()
        .take(16)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .filter_map(|item| {
            let text = match item {
                ConversationItem::Message(message)
                    if matches!(message.role, Role::User | Role::Assistant) =>
                {
                    format!(
                        "{:?}: {}",
                        message.role,
                        if message.role == Role::User {
                            openwebide_core::PromptContent::decode(&message.content).summary_text()
                        } else {
                            openwebide_core::strip_reasoning(&message.content).to_owned()
                        }
                    )
                }
                ConversationItem::ToolStep {
                    name,
                    summary,
                    result,
                    ..
                } => format!(
                    "Tool {name}: {summary}. Result: {}",
                    result.as_ref().map_or_else(
                        || "unfinished".into(),
                        |result| format!("success={}: {}", result.ok, result.summary)
                    )
                ),
                ConversationItem::Stopped { .. } => "Execution stopped by user".into(),
                ConversationItem::Notice { text, .. } => format!("Execution notice: {text}"),
                _ => return None,
            };
            Some(text.chars().take(2000).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
