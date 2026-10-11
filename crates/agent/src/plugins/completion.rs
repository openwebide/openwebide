//! General text model operations above provider and clock adapters.
use crate::model::ModelSource;
use openwebide_core::{
    ChatMessage, ChatRequest, ChatResponse, ModelRuntime, Role, StopReason,
    plugins::completion::{CompletionRequest, CompletionResponse, Profile},
};

pub async fn complete(
    source: &impl ModelSource,
    primary: ModelRuntime,
    input: CompletionRequest,
) -> Result<CompletionResponse, String> {
    input.validate()?;
    let runtime = match input.profile {
        Profile::Primary => primary,
        Profile::Fast => {
            let selection = primary
                .settings
                .fast
                .as_ref()
                .ok_or("No fast model is configured")?;
            source
                .runtime(selection)
                .await
                .map_err(|_| "Configured fast model is unavailable".to_owned())?
        }
    };
    let mut settings = runtime.settings;
    settings.tools = Some(false);
    let output = input
        .max_output_tokens
        .min(settings.max_output_tokens.unwrap_or(1024));
    if output == 0 {
        return Err("Model output is disabled".into());
    }
    settings.max_output_tokens = Some(output);
    let request = ChatRequest {
        connection_id: runtime.connection.id,
        model: runtime.connection.model,
        model_settings: settings,
        system_prompt: Some(input.system_prompt),
        tools: Vec::new(),
        messages: vec![ChatMessage {
            id: 0,
            session_id: 0,
            role: Role::User,
            content: input.prompt,
            created_at: 0,
            tool_calls: None,
            tool_call_id: None,
            usage: None,
        }],
    };
    let estimated = request
        .system_prompt
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(request.messages[0].content.len())
        .div_ceil(3)
        .saturating_add(64);
    let tokens = source.tokens(&request).await.unwrap_or(estimated);
    if tokens.saturating_add(output) > request.model_settings.context_limit.unwrap_or(32768) {
        return Err("Plugin completion input exceeds the configured model context".into());
    }
    let result = source
        .complete_with_timeout(&request, 30)
        .await
        .map_err(|_| "Configured model completion failed".to_owned())?;
    if result.stop_reason != StopReason::Complete {
        return Err("Plugin completion was cut off".into());
    }
    let ChatResponse::Text(text) = result.response else {
        return Err("Plugin completion returned tools instead of text".into());
    };
    if text.trim().is_empty() || text.len() > 16384 {
        return Err("Plugin completion returned empty or oversized text".into());
    }
    Ok(CompletionResponse { text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::{ChatCompletion, ModelSelection};
    use std::sync::Mutex;
    struct Source {
        calls: Mutex<Vec<(ChatRequest, u32)>>,
        result: ChatCompletion,
        exact: Option<usize>,
    }
    impl ModelSource for Source {
        async fn runtime(&self, selection: &ModelSelection) -> Result<ModelRuntime, String> {
            let mut runtime = runtime();
            runtime.connection.id = selection.server_id;
            runtime.connection.model = Some(selection.model.clone());
            runtime.settings.max_output_tokens = Some(32);
            Ok(runtime)
        }
        async fn tokens(&self, _: &ChatRequest) -> Option<usize> {
            self.exact
        }
        async fn complete_with_timeout(
            &self,
            request: &ChatRequest,
            timeout: u32,
        ) -> Result<ChatCompletion, String> {
            self.calls.lock().unwrap().push((request.clone(), timeout));
            Ok(self.result.clone())
        }
    }
    fn runtime() -> ModelRuntime {
        serde_json::from_value(serde_json::json!({"connection":{"id":1,"name":"primary","kind":"ollama","base_url":"http://localhost","model":"primary","enabled":true},"settings":{"context_limit":1024,"max_output_tokens":64,"tools":true,"fast":{"server_id":2,"model":"fast"}},"transport":{}})).unwrap()
    }
    fn source(text: &str) -> Source {
        Source {
            calls: Default::default(),
            exact: None,
            result: ChatCompletion {
                reasoning: String::new(),
                stop_reason: StopReason::Complete,
                preamble: String::new(),
                response: ChatResponse::Text(text.into()),
                usage: None,
            },
        }
    }
    fn input(profile: Profile) -> CompletionRequest {
        CompletionRequest {
            system_prompt: "Plugin-defined instructions".into(),
            prompt: "Plugin-defined input".into(),
            profile,
            max_output_tokens: 128,
        }
    }
    #[test]
    fn configured_profiles_share_bounded_text_only_completion() {
        futures::executor::block_on(async {
            for (profile, connection, limit) in [(Profile::Primary, 1, 64), (Profile::Fast, 2, 32)]
            {
                let source = source("Model text");
                assert_eq!(
                    complete(&source, runtime(), input(profile))
                        .await
                        .unwrap()
                        .text,
                    "Model text"
                );
                let calls = source.calls.lock().unwrap();
                let (request, timeout) = &calls[0];
                assert_eq!(request.connection_id, connection);
                assert_eq!(request.model_settings.max_output_tokens, Some(limit));
                assert_eq!(request.model_settings.tools, Some(false));
                assert!(request.tools.is_empty());
                assert_eq!(*timeout, 30);
                assert_eq!(
                    request.system_prompt.as_deref(),
                    Some("Plugin-defined instructions")
                );
            }
        });
    }
    #[test]
    fn invalid_inputs_and_context_overflow_never_start_a_model_call() {
        futures::executor::block_on(async {
            let mut source = source("Text");
            let mut bad = input(Profile::Primary);
            bad.max_output_tokens = 1025;
            assert!(complete(&source, runtime(), bad).await.is_err());
            let mut bad = input(Profile::Primary);
            bad.prompt = "X".repeat(32769);
            assert!(complete(&source, runtime(), bad).await.is_err());
            source.exact = Some(1024);
            assert!(
                complete(&source, runtime(), input(Profile::Primary))
                    .await
                    .is_err()
            );
            assert!(source.calls.lock().unwrap().is_empty());
            let mut runtime = runtime();
            runtime.settings.fast = None;
            assert!(
                complete(&source, runtime, input(Profile::Fast))
                    .await
                    .is_err()
            );
            assert!(serde_json::from_value::<CompletionRequest>(serde_json::json!({"system_prompt":"a","prompt":"b","profile":"primary","max_output_tokens":64,"connection_id":42})).is_err());
        });
    }
    #[test]
    fn empty_oversized_cut_off_and_tool_responses_are_not_successful_text() {
        futures::executor::block_on(async {
            for text in [" ".to_string(), "X".repeat(16385)] {
                assert!(
                    complete(&source(&text), runtime(), input(Profile::Primary))
                        .await
                        .is_err()
                );
            }
            let mut source = source("Partial");
            source.result.stop_reason = StopReason::Length;
            assert!(
                complete(&source, runtime(), input(Profile::Primary))
                    .await
                    .is_err()
            );
            source.result.stop_reason = StopReason::Complete;
            source.result.response = ChatResponse::ToolCalls(Vec::new());
            assert!(
                complete(&source, runtime(), input(Profile::Primary))
                    .await
                    .is_err()
            );
        });
    }
}
