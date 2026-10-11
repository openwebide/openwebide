//! Durable pending prompts, shared across execution hosts.
use crate::PromptContent;
use serde::{Deserialize, Serialize};

pub const MAX_QUEUED_PROMPTS: usize = 8;
pub const MAX_QUEUE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_QUEUED_PROMPT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedPromptKey {
    pub id: i64,
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedPrompt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_run: Option<i64>,
    pub id: i64,
    pub session_id: i64,
    pub revision: i64,
    /// Prepared immutable references, images and editor context.
    pub content: String,
    pub created_at: i64,
    #[serde(default)]
    pub guidance: bool,
}

impl QueuedPrompt {
    pub fn is_host_delivered(&self) -> bool {
        self.scheduled_task.is_some() || self.plugin_run.is_some()
    }
    pub fn key(&self) -> QueuedPromptKey {
        QueuedPromptKey {
            id: self.id,
            revision: self.revision,
        }
    }
}

pub fn validate_content(content: &str) -> Result<(), String> {
    if content.len() > MAX_QUEUED_PROMPT_BYTES {
        return Err("Each queued prompt must be at most 8 MiB.".into());
    }
    PromptContent::attachments(content)?;
    let prompt = PromptContent::decode(content);
    if prompt.text.trim().is_empty() && prompt.images.is_empty() {
        return Err("Enter a prompt or attach an image.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_prompts_require_valid_bounded_content() {
        assert!(validate_content(" ").is_err());
        assert!(validate_content("[Open WebIDE prompt]\ninvalid").is_err());
        assert!(validate_content(&"x".repeat(MAX_QUEUED_PROMPT_BYTES + 1)).is_err());
        assert!(validate_content("next task").is_ok());
    }
}
