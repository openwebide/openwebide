//! Bounded text completion; host identity and model credentials never enter plugins.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Primary,
    Fast,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRequest {
    pub system_prompt: String,
    pub prompt: String,
    #[serde(default)]
    pub profile: Profile,
    pub max_output_tokens: usize,
}
impl CompletionRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.prompt.trim().is_empty()
            || self.system_prompt.len().saturating_add(self.prompt.len()) > 32768
            || !(1..=1024).contains(&self.max_output_tokens)
        {
            return Err(
                "Plugin completion requires up to 32 KiB of input and 1–1024 output tokens".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionResponse {
    pub text: String,
}
