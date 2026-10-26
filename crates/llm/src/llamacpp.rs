use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt, stream};
use openwebide_core::{
    ChatCompletion, ChatRequest, ChatResponse, ModelInfo, ProviderKind, StopReason, ToolCall,
};
use serde_json::{Value, json};

use crate::sse::{SseField, sse_field};
use crate::{
    HttpClient, LineStream, LlmProvider, ProviderError, StreamChunk, StreamLine, ToolStreamChunk,
    ToolStreamMemo, UsageAcc, chat_messages, clock_now, completion_chunks, stream_error,
    tool_call_values, tool_messages, tools_wire, url_for,
};

/// Provider for a [llama.cpp](https://github.com/ggml-org/llama.cpp) server
/// (`llama-server`, OpenAI-compatible API).
pub struct LlamaCppProvider<C: HttpClient> {
    base_url: String,
    model: Option<String>,
    http: Arc<C>,
    tool_stream_memo: ToolStreamMemo,
}

impl<C: HttpClient> LlamaCppProvider<C> {
    pub fn new(base_url: impl Into<String>, model: Option<String>, http: C) -> Self {
        let base_url = base_url.into();
        let base_url = base_url
            .split(['?', '#'])
            .next()
            .unwrap_or(&base_url)
            .trim_end_matches('/');
        let base_url = base_url.strip_suffix("/v1").unwrap_or(base_url);
        Self {
            base_url: base_url.to_string(),
            model,
            http: Arc::new(http),
            tool_stream_memo: ToolStreamMemo::default(),
        }
    }
    pub fn with_tool_stream_memo(mut self, memo: ToolStreamMemo) -> Self {
        self.tool_stream_memo = memo;
        self
    }

    fn stream_tools(
        &self,
        request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send + 'static>> {
        let model = match crate::validated_model(request, self.model.as_deref()) {
            Ok(model) => model,
            Err(error) => return Box::pin(stream::once(async move { Err(error) })),
        };
        let mut body = json!({
            "model": model,
            "messages": tool_messages(request, ProviderKind::LlamaCpp),
            "stream": true,
            "tools": tools_wire(&request.tools),
            "stream_options": { "include_usage": true },
        });
        crate::apply_model_settings(&mut body, request, ProviderKind::LlamaCpp);
        let lines = LineStream::new(
            self.http
                .post_stream(&url_for(&self.base_url, "/v1/chat/completions"), &body),
        );
        crate::streaming::tool_stream(lines, request, LlamaCppToolParser::default())
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Read the usage/timing fields an OpenAI-compatible llama.cpp response
/// reports and overwrite only the ones present.
///
/// `usage` is `null` on intermediate streamed chunks (ignored); the final
/// chunk carries the real object. `timings.predicted_n` is used as the
/// completion count only when `usage` is absent, since `usage.completion_tokens`
/// is the authoritative count when both are present.
fn usage_fields(value: &Value, acc: &mut UsageAcc) {
    let usage = value.get("usage").filter(|u| !u.is_null());
    if let Some(usage) = usage {
        if let Some(n) = usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            acc.prompt = Some(usize::try_from(n).unwrap_or(usize::MAX));
        }
        if let Some(n) = usage
            .get("completion_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            acc.completion = Some(usize::try_from(n).unwrap_or(usize::MAX));
        }
    }
    if let Some(timings) = value.get("timings") {
        if let Some(ms) = timings
            .get("predicted_ms")
            .and_then(serde_json::Value::as_f64)
        {
            // Float-to-integer casts saturate; clamp negative durations to zero first.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let duration = ms.max(0.0).round() as u64;
            acc.eval_ms = Some(duration);
        }
        if usage.is_none()
            && let Some(n) = timings
                .get("predicted_n")
                .and_then(serde_json::Value::as_u64)
        {
            acc.completion = Some(usize::try_from(n).unwrap_or(usize::MAX));
        }
    }
}

impl<C: HttpClient + 'static> LlmProvider for LlamaCppProvider<C> {
    async fn request_tokens(&self, request: &ChatRequest) -> Option<usize> {
        openwebide_core::prompt::validate_request(request).ok()?;
        let image_tokens = openwebide_core::prompt::image_tokens(request);
        let text_request = openwebide_core::prompt::text_request(request);
        let rendered = self
            .http
            .post_json(
                &url_for(&self.base_url, "/apply-template"),
                &json!({
                    "model": request.model.as_ref().or(self.model.as_ref()),
                    "messages": tool_messages(&text_request, ProviderKind::LlamaCpp),
                    "tools": tools_wire(&request.tools),
                }),
            )
            .await
            .ok()?;
        let prompt = rendered.get("prompt")?.as_str()?;
        let counted = self
            .http
            .post_json(
                &url_for(&self.base_url, "/tokenize"),
                &json!({"content": prompt, "add_special": true, "parse_special": true}),
            )
            .await
            .ok()?;
        // Templates that ignore tools still need space for their schemas. Counting twice is conservative.
        let schemas = if request.tools.is_empty() {
            0
        } else {
            serde_json::to_vec(&tools_wire(&request.tools))
                .ok()?
                .len()
                .div_ceil(3)
        };
        let count = counted.get("tokens")?.as_array()?.len();
        (count > 0).then(|| {
            count
                .saturating_add(schemas)
                .saturating_add(32)
                .saturating_add(image_tokens)
        })
    }

    fn tool_stream_memo(&self) -> Option<ToolStreamMemo> {
        Some(self.tool_stream_memo.clone())
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::LlamaCpp
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let value = self
            .http
            .get_json(&url_for(&self.base_url, "/v1/models"))
            .await?;
        let data = value
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| {
                ProviderError::Parse("llama.cpp /v1/models: missing `data` array".into())
            })?;
        Ok(data
            .iter()
            .filter_map(|m| {
                m.get("id").and_then(|i| i.as_str()).map(|id| ModelInfo {
                    name: id.to_string(),
                })
            })
            .collect())
    }

    async fn chat(&self, request: &ChatRequest) -> Result<String, ProviderError> {
        let model = crate::validated_model(request, self.model.as_deref())?;
        let mut body = json!({
            "model": model,
            "messages": chat_messages(request, ProviderKind::LlamaCpp),
            "stream": false,
        });
        crate::apply_model_settings(&mut body, request, ProviderKind::LlamaCpp);
        let value = self
            .http
            .post_json(&url_for(&self.base_url, "/v1/chat/completions"), &body)
            .await?;
        value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                ProviderError::Parse(
                    "llama.cpp /v1/chat/completions: missing `choices[0].message.content`".into(),
                )
            })
            .map(str::to_string)
    }

    fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send + 'static>> {
        let model = match crate::validated_model(request, self.model.as_deref()) {
            Ok(model) => model,
            Err(error) => return Box::pin(stream::once(async move { Err(error) })),
        };
        let mut body = json!({
            "model": model,
            "messages": chat_messages(request, ProviderKind::LlamaCpp),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        crate::apply_model_settings(&mut body, request, ProviderKind::LlamaCpp);
        let url = url_for(&self.base_url, "/v1/chat/completions");
        let lines = LineStream::new(self.http.post_stream(&url, &body));
        crate::streaming::chat_stream(lines, request, parse_chat_line)
    }

    async fn chat_tools(&self, request: &ChatRequest) -> Result<ChatCompletion, ProviderError> {
        let model = crate::validated_model(request, self.model.as_deref())?;
        let mut body = json!({
            "model": model,
            "messages": tool_messages(request, ProviderKind::LlamaCpp),
            "stream": false,
            "tools": tools_wire(&request.tools),
        });
        crate::apply_model_settings(&mut body, request, ProviderKind::LlamaCpp);
        let mut acc = UsageAcc::new(request);
        acc.started = clock_now();
        let value = self
            .http
            .post_json(&url_for(&self.base_url, "/v1/chat/completions"), &body)
            .await?;
        acc.ended = clock_now();
        usage_fields(&value, &mut acc);
        let message = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .ok_or_else(|| {
                ProviderError::Parse(
                    "llama.cpp /v1/chat/completions: missing `choices[0].message`".into(),
                )
            })?;
        if let Some(calls) = tool_call_values(message) {
            let mut tool_calls = Vec::new();
            for call in calls {
                let function = call.get("function").ok_or_else(|| {
                    ProviderError::Parse("llama.cpp tool call missing `function`".into())
                })?;
                let name = function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ProviderError::Parse("llama.cpp tool call missing `name`".into())
                    })?
                    .to_string();
                // The OpenAI-compatible API returns `arguments` as a JSON
                // string, which is already the canonical form.
                let arguments = function
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let id = call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                acc.text.push_str(&name);
                acc.text.push_str(&arguments);
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
            let preamble = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            acc.text.push_str(&preamble);
            return Ok(ChatCompletion {
                reasoning: message
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                stop_reason: if value["choices"][0]["finish_reason"] == "length" {
                    StopReason::Length
                } else {
                    StopReason::Complete
                },
                response: ChatResponse::ToolCalls(tool_calls),
                preamble,
                usage: Some(acc.finish()),
            });
        }
        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        acc.text.push_str(&content);
        Ok(ChatCompletion {
            reasoning: message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stop_reason: if value["choices"][0]["finish_reason"] == "length" {
                StopReason::Length
            } else {
                StopReason::Complete
            },
            response: ChatResponse::Text(content),
            preamble: String::new(),
            usage: Some(acc.finish()),
        })
    }

    fn chat_tools_stream(
        &self,
        request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send + 'static>> {
        let provider = Self {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            http: self.http.clone(),
            tool_stream_memo: self.tool_stream_memo.clone(),
        };
        let request = request.clone();
        Box::pin(stream::once(async move {
            let mut first = None;
            if !provider.tool_stream_memo.unsupported() {
                let mut streaming = provider.stream_tools(&request);
                first = streaming.next().await;
                if !matches!(&first, Some(Err(ProviderError::Http(msg))) if msg.to_lowercase().contains("stream")) {
                    return Box::pin(stream::iter(first).chain(streaming)) as Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send>>;
                }
            }
            let chunks = match provider.chat_tools(&request).await {
                Ok(completion) => {
                    if first.is_some() { provider.tool_stream_memo.mark_unsupported(); }
                    completion_chunks(completion).into_iter().map(Ok).collect::<Vec<_>>()
                }
                Err(e) => vec![Err(e)],
            };
            Box::pin(stream::iter(chunks)) as Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send>>
        }).flatten())
    }

    async fn context_limit(&self, _model: Option<&str>) -> Result<Option<usize>, ProviderError> {
        // `llama-server`'s context size is fixed at startup (`-c`/`--ctx-size`)
        // and not tied to a model name, so there is nothing to resolve here.
        // Any error, including a 404 from a non-llama.cpp OpenAI-compatible
        // server, is treated as "no limit reported" rather than a hard error,
        // so the UI falls back quietly.
        match self.http.get_json(&url_for(&self.base_url, "/props")).await {
            Ok(value) => Ok(crate::discovery::positive(
                value.pointer("/default_generation_settings/n_ctx"),
            )
            .or_else(|| crate::discovery::positive(value.get("n_ctx")))),
            Err(_) => Ok(None),
        }
    }
}

/// Parse one SSE line of llama.cpp's streaming `/v1/chat/completions`
/// response.
fn parse_stream_line(line: &str) -> Result<StreamLine, ProviderError> {
    match sse_field(line) {
        SseField::Ignore => Ok(StreamLine::Skip),
        SseField::Error(value) => {
            let value = value.trim();
            let parsed: Value = serde_json::from_str(value).unwrap_or(Value::Null);
            let message = stream_error(&parsed).unwrap_or_else(|| value.to_string());
            Err(ProviderError::Http(message))
        }
        SseField::Data(data) => {
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(StreamLine::Done);
            }
            if data.is_empty() {
                return Ok(StreamLine::Skip);
            }
            let value: Value = serde_json::from_str(data).map_err(|e| {
                ProviderError::Parse(format!("llama.cpp stream: invalid JSON: {e}"))
            })?;
            if let Some(error) = stream_error(&value) {
                return Err(ProviderError::Http(error));
            }
            let choice = value
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|c| c.first());
            let content = choice
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty());
            let finished = choice
                .and_then(|c| c.get("finish_reason"))
                .is_some_and(|f| !f.is_null());
            match (finished, content) {
                (true, Some(content)) => Ok(StreamLine::Finished(Some(content.to_string()))),
                (true, None) => Ok(StreamLine::Finished(None)),
                (false, Some(content)) => Ok(StreamLine::Delta(content.to_string())),
                (false, None) => Ok(StreamLine::Skip),
            }
        }
    }
}

fn parse_chat_line(
    line: &str,
    acc: &mut UsageAcc,
) -> Result<crate::streaming::ParsedLine, ProviderError> {
    let event = parse_stream_line(line)?;
    let mut parsed = crate::streaming::ParsedLine {
        event,
        reasoning: None,
        stop: None,
    };
    if let Ok(value) =
        serde_json::from_str::<Value>(line.strip_prefix("data:").unwrap_or(line).trim())
    {
        usage_fields(&value, acc);
        parsed.reasoning = value["choices"][0]["delta"]["reasoning_content"]
            .as_str()
            .map(str::to_string);
        if value["choices"][0]["finish_reason"] == "length" {
            parsed.stop = Some(StopReason::Length);
        }
    }
    Ok(parsed)
}

#[derive(Default)]
struct LlamaCppToolParser {
    calls: BTreeMap<usize, PartialToolCall>,
}
impl crate::streaming::ToolParser for LlamaCppToolParser {
    fn parse(
        &mut self,
        line: &str,
        usage: &mut UsageAcc,
    ) -> Result<crate::streaming::ToolLine, ProviderError> {
        let parsed = parse_chat_line(line, usage)?;
        let mut result = crate::streaming::ToolLine {
            parsed,
            end: false,
            tool_activity: false,
        };
        if let SseField::Data(data) = sse_field(line) {
            if data.trim() == "[DONE]" {
                return Ok(result);
            }
            let value: Value = serde_json::from_str(data).map_err(|error| {
                ProviderError::Parse(format!("llama.cpp stream: invalid JSON: {error}"))
            })?;
            if let Some(values) = value["choices"][0]["delta"]["tool_calls"]
                .as_array()
                .filter(|values| !values.is_empty())
            {
                result.tool_activity = true;
                for (position, value) in values.iter().enumerate() {
                    let index = value["index"]
                        .as_u64()
                        .map(|index| usize::try_from(index).unwrap_or(usize::MAX))
                        .unwrap_or(position);
                    let call = self.calls.entry(index).or_default();
                    if call.id.is_empty() {
                        call.id = value["id"].as_str().unwrap_or_default().to_string();
                    }
                    if let Some(function) = value.get("function") {
                        if call.name.is_empty() {
                            call.name = function["name"].as_str().unwrap_or_default().to_string();
                        }
                        if let Some(arguments) = function["arguments"].as_str() {
                            call.arguments.push_str(arguments);
                        }
                    }
                }
            }
        }
        Ok(result)
    }
    fn finish(&mut self) -> Result<Vec<ToolCall>, ProviderError> {
        std::mem::take(&mut self.calls)
            .into_values()
            .map(|call| {
                if call.name.is_empty() {
                    return Err(ProviderError::Parse(
                        "llama.cpp tool call missing `name`".into(),
                    ));
                }
                Ok(ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::fake::{FakeHttpClient, FakeState};
    use futures::executor::block_on;
    use openwebide_core::{ChatMessage, Role, ToolDefinition, TurnTelemetry};

    #[test]
    fn request_tokenization_counts_rendered_history_and_reserves_schemas() {
        block_on(async {
            let (provider, state) = provider(FakeHttpClient::new());
            state.push(Ok(json!({"prompt":"rendered chat"})));
            state.push(Ok(json!({"tokens":[1,2,3,4]})));
            let mut request = request(None, Some("rules"));
            request.tools.push(read_file_tool());
            assert!(provider.request_tokens(&request).await.unwrap() > 36);
            {
                let calls = state.calls.lock().unwrap();
                assert!(calls[0].url.ends_with("/apply-template"));
                assert_eq!(
                    calls[0].body.as_ref().unwrap()["messages"][0]["content"],
                    "rules"
                );
                assert!(calls[1].url.ends_with("/tokenize"));
                assert_eq!(calls[1].body.as_ref().unwrap()["content"], "rendered chat");
            }
            state.push(Err(ProviderError::Http("unsupported".into())));
            assert!(provider.request_tokens(&request).await.is_none());
        });
    }

    #[test]
    fn oversized_usage_keeps_telemetry_saturated() {
        let mut acc = UsageAcc::new(&request(None, None));
        usage_fields(
            &json!({"usage": {"prompt_tokens": u64::MAX, "completion_tokens": 1}}),
            &mut acc,
        );
        let usage = acc.finish();
        let mut telemetry = openwebide_core::SessionTelemetry::default();
        telemetry.record_turn(&usage);
        telemetry.record_turn(&usage);
        assert_eq!(telemetry.context_tokens, usize::MAX);
        assert_eq!(telemetry.total_prompt_tokens, usize::MAX);
        assert_eq!(telemetry.total_completion_tokens, 2);
    }

    const BASE: &str = "http://localhost:8080";

    fn provider(http: FakeHttpClient) -> (LlamaCppProvider<FakeHttpClient>, Arc<FakeState>) {
        let state = http.state();
        (
            LlamaCppProvider::new(BASE, Some("qwen2.5-coder".into()), http),
            state,
        )
    }

    fn message(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            id: 1,
            session_id: 1,
            role,
            content: content.into(),
            created_at: 0,
            tool_calls: None,
            tool_call_id: None,
            usage: None,
        }
    }

    fn request(model: Option<&str>, system: Option<&str>) -> ChatRequest {
        ChatRequest {
            model_settings: Default::default(),
            connection_id: 1,
            system_prompt: system.map(str::to_string),
            model: model.map(str::to_string),
            messages: vec![message(Role::User, "hi")],
            tools: vec![],
        }
    }

    fn read_file_tool() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    #[test]
    fn base_url_normalization_applies_to_all_endpoints() {
        for (base_url, root) in [
            ("http://h", "http://h"),
            ("http://h/", "http://h"),
            ("http://h/v1", "http://h"),
            ("http://h/v1/", "http://h"),
            ("http://h/proxy/v1", "http://h/proxy"),
            ("http://h/v10", "http://h/v10"),
            ("http://h/api/v1x", "http://h/api/v1x"),
            ("http://h/v1/v1", "http://h/v1"),
            ("http://h/proxy/v1/?key=old#fragment", "http://h/proxy"),
        ] {
            let http = FakeHttpClient::new();
            let state = http.state();
            let provider = LlamaCppProvider::new(base_url, Some("model".into()), http);
            let request = request(None, None);
            state.push(Ok(json!({"data": []})));
            block_on(provider.list_models()).unwrap();
            for _ in 0..2 {
                state.push(Ok(json!({"choices": [{"message": {"content": "hello"}}]})));
            }
            block_on(provider.chat(&request)).unwrap();
            block_on(provider.chat_tools(&request)).unwrap();
            state.push_stream(vec!["data: [DONE]\n"]);
            let chunks: Vec<_> = block_on(provider.chat_stream(&request).collect());
            assert!(chunks.iter().all(Result::is_ok));
            state.push_stream(vec!["data: [DONE]\n"]);
            let chunks: Vec<_> = block_on(provider.chat_tools_stream(&request).collect());
            assert!(chunks.iter().all(Result::is_ok));
            state.push(Ok(json!({"default_generation_settings": {"n_ctx": 4096}})));
            assert_eq!(block_on(provider.context_limit(None)).unwrap(), Some(4096));

            let calls = state.calls.lock().unwrap();
            let urls: Vec<_> = calls.iter().map(|call| call.url.as_str()).collect();
            assert_eq!(
                urls,
                vec![
                    format!("{root}/v1/models"),
                    format!("{root}/v1/chat/completions"),
                    format!("{root}/v1/chat/completions"),
                    format!("{root}/v1/chat/completions"),
                    format!("{root}/v1/chat/completions"),
                    format!("{root}/props"),
                ],
                "base URL: {base_url}"
            );
        }
    }

    #[test]
    fn list_models_parses_openai_data() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "data": [ { "id": "qwen2.5-coder" }, { "id": "mistral" } ]
        })));

        let models = block_on(provider.list_models()).unwrap();

        assert_eq!(
            models,
            vec![
                ModelInfo {
                    name: "qwen2.5-coder".into()
                },
                ModelInfo {
                    name: "mistral".into()
                },
            ]
        );
        assert_eq!(
            state.calls.lock().unwrap()[0].url,
            format!("{BASE}/v1/models")
        );
    }

    #[test]
    fn list_models_rejects_missing_data() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({})));

        assert!(matches!(
            block_on(provider.list_models()),
            Err(ProviderError::Parse(_))
        ));
    }

    #[test]
    fn chat_sends_model_messages_and_system_prompt() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "choices": [ { "message": { "role": "assistant", "content": "hello" } } ]
        })));

        assert_eq!(
            block_on(provider.chat(&request(None, Some("be brief")))).unwrap(),
            "hello"
        );

        let calls = state.calls.lock().unwrap();
        assert_eq!(calls[0].url, format!("{BASE}/v1/chat/completions"));
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["model"], "qwen2.5-coder");
        assert_eq!(body["stream"], false);
        assert_eq!(
            body["messages"][0],
            json!({ "role": "system", "content": "be brief" })
        );
        assert_eq!(
            body["messages"][1],
            json!({ "role": "user", "content": "hi" })
        );
    }

    #[test]
    fn chat_without_any_model_is_an_error() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = LlamaCppProvider::new(BASE, None, http);

        assert!(matches!(
            block_on(provider.chat(&request(None, None))),
            Err(ProviderError::NoModel)
        ));
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn http_errors_propagate() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Err(ProviderError::Http("500 Internal Server Error".into())));

        assert!(matches!(
            block_on(provider.chat(&request(None, None))),
            Err(ProviderError::Http(_))
        ));
    }

    /// Run a stream to completion, splitting deltas from the (at most one)
    /// usage chunk.
    fn run_stream(
        provider: &LlamaCppProvider<FakeHttpClient>,
        req: &ChatRequest,
    ) -> (Vec<String>, Vec<TurnTelemetry>, Vec<ProviderError>) {
        let items: Vec<Result<StreamChunk, ProviderError>> =
            block_on(provider.chat_stream(req).collect());
        let mut deltas = Vec::new();
        let mut usages = Vec::new();
        let mut errors = Vec::new();
        for item in items {
            match item {
                Ok(StreamChunk::Delta(d)) => deltas.push(d),
                Ok(StreamChunk::Usage(u)) => usages.push(u),
                Ok(StreamChunk::Reasoning(_) | StreamChunk::Stop(_)) => {}
                Err(e) => errors.push(e),
            }
        }
        (deltas, usages, errors)
    }

    #[test]
    fn chat_stream_yields_deltas_until_done() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n",
            "data: [DONE]\n",
        ]);

        let (deltas, _usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["Hel", "lo"]);
        assert!(errors.is_empty());
        let calls = state.calls.lock().unwrap();
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
    }

    #[test]
    fn chat_stream_ignores_comments_and_meta_deltas() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            ": keep-alive\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
            "data: [DONE]\n",
        ]);

        let (deltas, _usages, _errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
    }

    #[test]
    fn chat_stream_intermediate_null_usage_is_ignored() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}],\"usage\":null}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2},\"timings\":{\"predicted_ms\":150.4}}\n",
            "data: [DONE]\n",
        ]);

        let (deltas, usages, _errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].prompt_tokens, 10);
        assert_eq!(usages[0].completion_tokens, 2);
        assert_eq!(usages[0].eval_duration_ms, 150);
        assert!(!usages[0].estimated);
    }

    #[test]
    fn chat_stream_reports_provider_errors() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "data: {\"error\":{\"message\":\"context length exceeded\"}}\n",
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert!(usages.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ProviderError::Http(msg) if msg.contains("context length")));
    }

    #[test]
    fn chat_stream_ignores_event_id_retry_fields() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "event: message\n",
            "id: 1\n",
            "retry: 3000\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "data: [DONE]\n",
        ]);

        let (deltas, _usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert!(errors.is_empty());
    }

    #[test]
    fn chat_stream_data_field_without_space() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data:{\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "data:[DONE]\n",
        ]);

        let (deltas, _usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert!(errors.is_empty());
    }

    #[test]
    fn chat_stream_surfaces_error_field() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "error: {\"code\":500,\"message\":\"boom\"}\n",
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert!(usages.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ProviderError::Http(msg) if msg.contains("boom")));
    }

    #[test]
    fn chat_stream_bare_json_error_line_is_an_error() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec!["{\"error\":{\"message\":\"bare json error\"}}\n"]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert!(deltas.is_empty());
        assert!(usages.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ProviderError::Http(msg) if msg.contains("bare json error")));
    }

    #[test]
    fn chat_stream_without_done_is_incomplete() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert!(usages.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ProviderError::Incomplete));
    }

    #[test]
    fn finish_reason_then_eof_is_complete() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n",
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a"]);
        assert!(errors.is_empty());
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].prompt_tokens, 10);
        assert_eq!(usages[0].completion_tokens, 2);
        assert!(!usages[0].estimated);
    }

    #[test]
    fn chat_stream_without_any_model_is_an_error() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = LlamaCppProvider::new(BASE, None, http);

        let (_deltas, _usages, errors) = run_stream(&provider, &request(None, None));

        assert!(matches!(errors[0], ProviderError::NoModel));
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn chat_tools_returns_tool_calls() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "choices": [ {
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_abc",
                            "type": "function",
                            "function": { "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}" }
                        }
                    ]
                }
            } ],
            "usage": { "prompt_tokens": 40, "completion_tokens": 8 },
            "timings": { "predicted_ms": 200.0 }
        })));

        let mut req = request(None, None);
        req.tools = vec![read_file_tool()];
        let completion = block_on(provider.chat_tools(&req)).unwrap();

        // The provider-supplied id and string arguments are preserved as-is.
        assert_eq!(
            completion.response,
            ChatResponse::ToolCalls(vec![ToolCall {
                id: "call_abc".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }])
        );
        let usage = completion.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 40);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.eval_duration_ms, 200);
        assert!(!usage.estimated);
        let calls = state.calls.lock().unwrap();
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["stream"], false);
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn chat_tools_returns_text_when_no_calls() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "choices": [ { "message": { "role": "assistant", "content": "all done" } } ]
        })));

        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        assert_eq!(completion.response, ChatResponse::Text("all done".into()));
        // No `usage` on the response: the estimator filled the counts in.
        assert!(completion.usage.unwrap().estimated);
    }

    #[test]
    fn chat_tools_empty_tool_calls_is_text() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "choices": [ {
                "message": {
                    "role": "assistant",
                    "content": "final answer",
                    "tool_calls": []
                }
            } ]
        })));

        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        assert_eq!(
            completion.response,
            ChatResponse::Text("final answer".into())
        );
    }

    #[test]
    fn chat_tools_sends_tool_call_and_result_messages() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "choices": [ { "message": { "role": "assistant", "content": "fixed it" } } ]
        })));

        let assistant = ChatMessage {
            id: 2,
            session_id: 1,
            role: Role::Assistant,
            content: String::new(),
            created_at: 0,
            tool_calls: Some(vec![ToolCall {
                id: "call_abc".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }]),
            tool_call_id: None,
            usage: None,
        };
        let tool_result = ChatMessage {
            id: 3,
            session_id: 1,
            role: Role::Tool,
            content: "fn main() {}".into(),
            created_at: 0,
            tool_calls: None,
            tool_call_id: Some("call_abc".into()),
            usage: None,
        };
        let req = ChatRequest {
            model_settings: Default::default(),
            connection_id: 1,
            system_prompt: None,
            model: None,
            messages: vec![message(Role::User, "fix it"), assistant, tool_result],
            tools: vec![read_file_tool()],
        };
        block_on(provider.chat_tools(&req)).unwrap();

        let calls = state.calls.lock().unwrap();
        let body = calls[0].body.as_ref().unwrap();
        // Assistant message carries id, type, and string arguments (OpenAI
        // wire format).
        assert_eq!(
            body["messages"][1],
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_abc",
                        "type": "function",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}" }
                    }
                ]
            })
        );
        // Tool result carries the tool_call_id (OpenAI wire format).
        assert_eq!(
            body["messages"][2],
            json!({ "role": "tool", "tool_call_id": "call_abc", "content": "fn main() {}" })
        );
    }

    #[test]
    fn chat_tools_without_any_model_is_an_error() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = LlamaCppProvider::new(BASE, None, http);

        assert!(matches!(
            block_on(provider.chat_tools(&request(None, None))),
            Err(ProviderError::NoModel)
        ));
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn chat_tools_with_no_usage_estimates_non_zero_counts() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "choices": [ { "message": { "role": "assistant", "content": "a fairly long answer here" } } ]
        })));

        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        let usage = completion.usage.unwrap();
        assert!(usage.estimated);
        assert!(usage.prompt_tokens > 0);
        assert!(usage.completion_tokens > 0);
    }

    #[test]
    fn context_limit_reads_props() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "default_generation_settings": { "n_ctx": 4096 }
        })));

        let limit = block_on(provider.context_limit(None)).unwrap();
        assert_eq!(limit, Some(4096));
        assert_eq!(state.calls.lock().unwrap()[0].url, format!("{BASE}/props"));

        state.push(Ok(json!({ "n_ctx": 8192 })));
        assert_eq!(block_on(provider.context_limit(None)).unwrap(), Some(8192));

        state.push(Ok(json!({ "n_ctx": 0 })));
        assert_eq!(block_on(provider.context_limit(None)).unwrap(), None);
    }

    #[test]
    fn context_limit_maps_http_error_to_none() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Err(ProviderError::Http("404 Not Found".into())));

        assert_eq!(block_on(provider.context_limit(None)).unwrap(), None);
    }
    fn tool_items(
        provider: &LlamaCppProvider<FakeHttpClient>,
        req: &ChatRequest,
    ) -> Vec<Result<ToolStreamChunk, ProviderError>> {
        block_on(provider.chat_tools_stream(req).collect())
    }

    #[test]
    fn tool_stream_reassembles_interleaved_calls_and_trailing_usage() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"data: {"choices":[{"delta":{"content":"Checking","tool_calls":[{"index":1,"id":"second","function":{"name":"write_file","arguments":"{\"path\":"}}]}}]}
"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"first","function":{"name":"read_file","arguments":"{}"}},{"index":1,"id":"ignored","function":{"name":"ignored","arguments":"\"a\"}"}}]}}]}
"#,
            r#"data: {"choices":[{"finish_reason":"tool_calls"}]}
"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":8},"timings":{"predicted_ms":123}}
"#,
        ]);
        let mut req = request(None, None);
        req.tools = vec![read_file_tool()];
        let items = tool_items(&provider, &req);
        assert!(matches!(
            items[1],
            Ok(ToolStreamChunk::Stop(StopReason::Complete))
        ));
        assert_eq!(items.len(), 4);
        assert_eq!(
            items[0].as_ref().unwrap(),
            &ToolStreamChunk::Delta("Checking".into())
        );
        assert_eq!(
            items[2].as_ref().unwrap(),
            &ToolStreamChunk::Usage(TurnTelemetry {
                context: None,
                prompt_tokens: 12,
                completion_tokens: 8,
                eval_duration_ms: 123,
                estimated: false
            })
        );
        assert_eq!(
            items[3].as_ref().unwrap(),
            &ToolStreamChunk::Response(ChatResponse::ToolCalls(vec![
                ToolCall {
                    id: "first".into(),
                    name: "read_file".into(),
                    arguments: "{}".into()
                },
                ToolCall {
                    id: "second".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"a"}"#.into()
                },
            ]))
        );
        let body = state.calls.lock().unwrap()[0].body.clone().unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({"include_usage":true}));
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn tool_stream_missing_indexes_use_array_positions_and_defaults() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"data:{"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_file"}},{"function":{"name":"git_status"}}]}}]}
"#,
            "data:[DONE]\n",
        ]);
        let items = tool_items(&provider, &request(None, None));
        assert!(matches!(
            items[0],
            Ok(ToolStreamChunk::Stop(StopReason::Complete))
        ));
        assert_eq!(items.len(), 3);
        assert!(matches!(items[1], Ok(ToolStreamChunk::Usage(_))));
        assert_eq!(
            items[2].as_ref().unwrap(),
            &ToolStreamChunk::Response(ChatResponse::ToolCalls(vec![
                ToolCall {
                    id: "".into(),
                    name: "read_file".into(),
                    arguments: "".into()
                },
                ToolCall {
                    id: "".into(),
                    name: "git_status".into(),
                    arguments: "".into()
                },
            ]))
        );
    }

    #[test]
    fn tool_stream_text_only_and_incomplete() {
        for complete in [false, true] {
            let (provider, state) = provider(FakeHttpClient::new());
            let mut lines = vec![
                r#"data: {"choices":[{"delta":{"content":"a"}}]}
"#,
                r#"data: {"choices":[{"delta":{"content":"b"}}]}
"#,
            ];
            if complete {
                lines.push("data: [DONE]\n");
            }
            state.push_stream(lines);
            let items = tool_items(&provider, &request(None, None));
            assert_eq!(
                items[0].as_ref().unwrap(),
                &ToolStreamChunk::Delta("a".into())
            );
            assert_eq!(
                items[1].as_ref().unwrap(),
                &ToolStreamChunk::Delta("b".into())
            );
            if complete {
                assert_eq!(items.len(), 5);
                assert!(matches!(
                    items[2],
                    Ok(ToolStreamChunk::Stop(StopReason::Complete))
                ));
                assert!(matches!(items[3], Ok(ToolStreamChunk::Usage(_))));
                assert_eq!(
                    items[4].as_ref().unwrap(),
                    &ToolStreamChunk::Response(ChatResponse::Text("ab".into()))
                );
            } else {
                assert_eq!(items.len(), 3);
                assert!(matches!(items[2], Err(ProviderError::Incomplete)));
            }
        }
    }

    #[test]
    fn tool_stream_missing_name_is_parse_error() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}
"#,
            "data: [DONE]\n",
        ]);
        let items = tool_items(&provider, &request(None, None));
        assert_eq!(items.len(), 1);
        assert!(
            matches!(&items[0], Err(ProviderError::Parse(msg)) if msg == "llama.cpp tool call missing `name`")
        );
    }

    #[test]
    fn tool_stream_retry_sets_shared_memo_and_skips_future_streams() {
        let memos = crate::ToolStreamMemos::default();
        let mut connection = openwebide_core::Connection {
            id: 7,
            name: "test".into(),
            kind: ProviderKind::LlamaCpp,
            base_url: BASE.into(),
            model: None,
            enabled: true,
            context_limit: None,
            tool_stream_unsupported: false,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        };
        let memo = memos.get_or_insert(&connection);
        let (provider, state) = provider(FakeHttpClient::new());
        let provider = provider.with_tool_stream_memo(memo.clone());
        state.push_stream_error(ProviderError::Http(
            "500: Cannot use tools with STREAM".into(),
        ));
        let value = json!({"choices":[{"message":{"content":"Checking","tool_calls":[{"id":"c1","function":{"name":"read_file","arguments":"{}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":5},"timings":{"predicted_ms":50}});
        state.push(Ok(value.clone()));
        let expected = completion_chunks(ChatCompletion {
            reasoning: String::new(),
            stop_reason: openwebide_core::StopReason::Complete,
            preamble: "Checking".into(),
            response: ChatResponse::ToolCalls(vec![ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }]),
            usage: Some(TurnTelemetry {
                context: None,
                prompt_tokens: 10,
                completion_tokens: 5,
                eval_duration_ms: 50,
                estimated: false,
            }),
        });
        let items = tool_items(&provider, &request(None, None));
        assert_eq!(
            items.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            expected
        );
        assert!(memo.unsupported());
        assert!(memos.get_or_insert(&connection).unsupported());
        connection.id = 8;
        assert!(!memos.get_or_insert(&connection).unsupported());
        connection.id = 7;
        let mut other = connection.clone();
        other.base_url = "http://other".into();
        assert!(!memos.get_or_insert(&other).unsupported());
        connection.tool_stream_unsupported = true;
        state.push(Ok(value));
        let provider = LlamaCppProvider {
            base_url: provider.base_url,
            model: provider.model,
            http: provider.http,
            tool_stream_memo: memos.get_or_insert(&connection),
        };
        assert_eq!(
            tool_items(&provider, &request(None, None))
                .into_iter()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            expected
        );
        let calls = state.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].body.as_ref().unwrap()["stream"], true);
        assert_eq!(calls[1].body.as_ref().unwrap()["stream"], false);
        assert_eq!(calls[2].body.as_ref().unwrap()["stream"], false);
    }

    #[test]
    fn tool_stream_nonmatching_error_does_not_retry() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream_error(ProviderError::Http("500: bad model".into()));
        let items = tool_items(&provider, &request(None, None));
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Err(ProviderError::Http(msg)) if msg == "500: bad model"));
        assert_eq!(state.calls.lock().unwrap().len(), 1);
        assert!(!provider.tool_stream_memo.unsupported());
    }

    #[test]
    fn tool_stream_failed_retry_leaves_memo_unset() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream_error(ProviderError::Http("Cannot use tools with stream".into()));
        state.push(Err(ProviderError::Http("retry failed".into())));
        let items = tool_items(&provider, &request(None, None));
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Err(ProviderError::Http(msg)) if msg == "retry failed"));
        assert_eq!(state.calls.lock().unwrap().len(), 2);
        assert!(!provider.tool_stream_memo.unsupported());
    }

    #[test]
    fn tool_stream_error_after_delta_never_retries() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"data: {"choices":[{"delta":{"content":"a"}}]}
"#,
            r#"error: {"error":"Cannot stream"}
"#,
        ]);
        let items = tool_items(&provider, &request(None, None));
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], Ok(ToolStreamChunk::Delta(_))));
        assert!(matches!(items[1], Err(ProviderError::Http(_))));
        assert_eq!(state.calls.lock().unwrap().len(), 1);
    }
    #[test]
    fn reasoning_and_stop_reason_stream_in_order() {
        for length in [false, true] {
            for tools in [false, true] {
                let (provider, state) = provider(FakeHttpClient::new());
                state.push_stream(vec![
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"r\"}}]}\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n",
                    if length {
                        "data: {\"choices\":[{\"finish_reason\":\"length\"}]}\n"
                    } else {
                        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n"
                    },
                    "data: [DONE]\n",
                ]);
                let reason = if length {
                    StopReason::Length
                } else {
                    StopReason::Complete
                };
                let req = request(None, None);
                if tools {
                    let chunks = tool_items(&provider, &req)
                        .into_iter()
                        .map(Result::unwrap)
                        .collect::<Vec<_>>();
                    assert_eq!(chunks.len(), 5);
                    assert_eq!(chunks[0], ToolStreamChunk::Reasoning("r".into()));
                    assert_eq!(chunks[1], ToolStreamChunk::Delta("answer".into()));
                    assert_eq!(chunks[2], ToolStreamChunk::Stop(reason));
                    assert!(matches!(chunks[3], ToolStreamChunk::Usage(_)));
                    assert_eq!(
                        chunks[4],
                        ToolStreamChunk::Response(ChatResponse::Text("answer".into()))
                    );
                } else {
                    let chunks = block_on(provider.chat_stream(&req).collect::<Vec<_>>())
                        .into_iter()
                        .map(Result::unwrap)
                        .collect::<Vec<_>>();
                    assert_eq!(chunks.len(), 4);
                    assert!(matches!(&chunks[0], StreamChunk::Reasoning(r) if r == "r"));
                    assert!(matches!(&chunks[1], StreamChunk::Delta(d) if d == "answer"));
                    assert!(matches!(chunks[2], StreamChunk::Stop(r) if r == reason));
                    assert!(matches!(chunks[3], StreamChunk::Usage(_)));
                }
            }
        }
    }

    #[test]
    fn nonstream_completion_preserves_reasoning_and_length() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({"choices":[{"message":{"content":"answer","reasoning_content":"r"},"finish_reason":"length"}]})));
        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        let chunks = crate::completion_chunks(completion);
        assert_eq!(chunks[0], ToolStreamChunk::Reasoning("r".into()));
        assert_eq!(chunks[1], ToolStreamChunk::Delta("answer".into()));
        assert_eq!(chunks[2], ToolStreamChunk::Stop(StopReason::Length));
        assert!(matches!(chunks[3], ToolStreamChunk::Usage(_)));
    }
}
