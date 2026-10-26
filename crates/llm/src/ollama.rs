use std::pin::Pin;

use futures::{Stream, stream};
use openwebide_core::{
    ChatCompletion, ChatRequest, ChatResponse, ModelInfo, ProviderKind, StopReason, ToolCall,
};
use serde_json::{Value, json};

use crate::sse::{SseField, sse_field};
use crate::{
    HttpClient, LineStream, LlmProvider, ProviderError, StreamChunk, StreamLine, ToolStreamChunk,
    UsageAcc, chat_messages, clock_now, round_ns_to_ms, stream_error, tool_call_values,
    tool_messages, tools_wire, url_for,
};

/// Provider for [Ollama](https://ollama.com).
///
/// Talks to the Ollama HTTP API (`/api/tags`, `/api/chat`).
pub struct OllamaProvider<C: HttpClient> {
    base_url: String,
    model: Option<String>,
    http: C,
    num_ctx: Option<usize>,
}

impl<C: HttpClient> OllamaProvider<C> {
    pub fn new(base_url: impl Into<String>, model: Option<String>, http: C) -> Self {
        Self {
            base_url: base_url.into(),
            model,
            http,
            num_ctx: None,
        }
    }

    /// Set the context window sent as `options.num_ctx` on every request.
    /// `None` omits the `options` key entirely.
    pub fn with_num_ctx(mut self, n: Option<usize>) -> Self {
        self.num_ctx = n;
        self
    }
}

/// Read the usage fields Ollama reports and overwrite only the ones present.
fn usage_fields(value: &Value, acc: &mut UsageAcc) {
    if let Some(n) = value
        .get("prompt_eval_count")
        .and_then(serde_json::Value::as_u64)
    {
        acc.prompt = Some(usize::try_from(n).unwrap_or(usize::MAX));
    }
    if let Some(n) = value.get("eval_count").and_then(serde_json::Value::as_u64) {
        acc.completion = Some(usize::try_from(n).unwrap_or(usize::MAX));
    }
    if let Some(ns) = value
        .get("eval_duration")
        .and_then(serde_json::Value::as_u64)
    {
        acc.eval_ms = Some(round_ns_to_ms(ns));
    }
}

impl<C: HttpClient> LlmProvider for OllamaProvider<C> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let value = self
            .http
            .get_json(&url_for(&self.base_url, "/api/tags"))
            .await?;
        let models = value
            .get("models")
            .and_then(|m| m.as_array())
            .ok_or_else(|| {
                ProviderError::Parse("Ollama /api/tags: missing `models` array".into())
            })?;
        Ok(models
            .iter()
            .filter_map(|m| {
                m.get("name")
                    .and_then(|n| n.as_str())
                    .map(|name| ModelInfo {
                        name: name.to_string(),
                    })
            })
            .collect())
    }

    async fn chat(&self, request: &ChatRequest) -> Result<String, ProviderError> {
        let model = crate::validated_model(request, self.model.as_deref())?;
        let mut body = json!({
            "model": model,
            "messages": chat_messages(request, ProviderKind::Ollama),
            "stream": false,
        });
        if let Some(n) = self.num_ctx {
            body["options"] = json!({ "num_ctx": n });
        }
        crate::apply_model_settings(&mut body, request, ProviderKind::Ollama);
        let value = self
            .http
            .post_json(&url_for(&self.base_url, "/api/chat"), &body)
            .await?;
        value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                ProviderError::Parse("Ollama /api/chat: missing `message.content`".into())
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
            "messages": chat_messages(request, ProviderKind::Ollama),
            "stream": true,
        });
        if let Some(n) = self.num_ctx {
            body["options"] = json!({ "num_ctx": n });
        }
        crate::apply_model_settings(&mut body, request, ProviderKind::Ollama);
        let url = url_for(&self.base_url, "/api/chat");
        let lines = LineStream::new(self.http.post_stream(&url, &body));
        crate::streaming::chat_stream(lines, request, parse_chat_line)
    }

    async fn chat_tools(&self, request: &ChatRequest) -> Result<ChatCompletion, ProviderError> {
        let model = crate::validated_model(request, self.model.as_deref())?;
        let mut body = json!({
            "model": model,
            "messages": tool_messages(request, ProviderKind::Ollama),
            "stream": false,
            "tools": tools_wire(&request.tools),
        });
        if let Some(n) = self.num_ctx {
            body["options"] = json!({ "num_ctx": n });
        }
        crate::apply_model_settings(&mut body, request, ProviderKind::Ollama);
        let mut acc = UsageAcc::new(request);
        acc.started = clock_now();
        let value = self
            .http
            .post_json(&url_for(&self.base_url, "/api/chat"), &body)
            .await?;
        acc.ended = clock_now();
        usage_fields(&value, &mut acc);
        let message = value
            .get("message")
            .ok_or_else(|| ProviderError::Parse("Ollama /api/chat: missing `message`".into()))?;
        if let Some(calls) = tool_call_values(message) {
            let tool_calls = parse_tool_calls(calls, 0)?;
            for call in &tool_calls {
                acc.text.push_str(&call.name);
                acc.text.push_str(&call.arguments);
            }
            let preamble = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            acc.text.push_str(&preamble);
            return Ok(ChatCompletion {
                reasoning: message
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                stop_reason: if value["done_reason"] == "length" {
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
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stop_reason: if value["done_reason"] == "length" {
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
        let model = match crate::validated_model(request, self.model.as_deref()) {
            Ok(model) => model,
            Err(error) => return Box::pin(stream::once(async move { Err(error) })),
        };
        let mut body = json!({
            "model": model,
            "messages": tool_messages(request, ProviderKind::Ollama),
            "stream": true,
            "tools": tools_wire(&request.tools),
        });
        if let Some(n) = self.num_ctx {
            body["options"] = json!({ "num_ctx": n });
        }
        crate::apply_model_settings(&mut body, request, ProviderKind::Ollama);
        let lines = LineStream::new(
            self.http
                .post_stream(&url_for(&self.base_url, "/api/chat"), &body),
        );
        crate::streaming::tool_stream(lines, request, OllamaToolParser::default())
    }

    async fn context_limit(&self, model: Option<&str>) -> Result<Option<usize>, ProviderError> {
        let Ok(model) = crate::request_model(model, self.model.as_deref()) else {
            return Ok(None);
        };
        let value = self
            .http
            .post_json(
                &url_for(&self.base_url, "/api/show"),
                &json!({ "model": model }),
            )
            .await?;
        // A Modelfile `num_ctx` is the real runtime window Ollama loads the
        // model with. `model_info.<arch>.context_length` is the model's
        // trained maximum, not what Ollama actually runs at (its default
        // window is far smaller unless `num_ctx` is set), so it is
        // deliberately not used as a fallback here (user decision).
        if let Some(params) = value.get("parameters").and_then(|v| v.as_str()) {
            for line in params.lines() {
                if let Some(rest) = line.trim().strip_prefix("num_ctx")
                    && let Ok(n) = rest.trim().parse::<usize>()
                    && n > 0
                {
                    return Ok(Some(n));
                }
            }
        }
        if let Ok(running) = self
            .http
            .get_json(&url_for(&self.base_url, "/api/ps"))
            .await
            && let Some(entry) = crate::discovery::running_model(&running, model)
        {
            return Ok(crate::discovery::positive(entry.get("context_length")));
        }
        Ok(None)
    }
}

fn parse_tool_calls(values: &[Value], start: usize) -> Result<Vec<ToolCall>, ProviderError> {
    values
        .iter()
        .enumerate()
        .map(|(i, call)| {
            let function = call.get("function").ok_or_else(|| {
                ProviderError::Parse("Ollama tool call missing `function`".into())
            })?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ProviderError::Parse("Ollama tool call missing `name`".into()))?
                .to_string();
            let arguments = match function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}))
            {
                Value::String(s) => s,
                other => other.to_string(),
            };
            Ok(ToolCall {
                id: format!("call_{}", start + i),
                name,
                arguments,
            })
        })
        .collect()
}

/// Parse one NDJSON line of Ollama's streaming `/api/chat` response.
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
            let value: Value = serde_json::from_str(data)
                .map_err(|e| ProviderError::Parse(format!("Ollama stream: invalid JSON: {e}")))?;
            if let Some(error) = stream_error(&value) {
                return Err(ProviderError::Http(error));
            }
            let content = value
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str());
            let done = value.get("done").and_then(serde_json::Value::as_bool) == Some(true);
            if done {
                // A final line may carry a last delta alongside `done: true`.
                return match content {
                    Some(content) if !content.is_empty() => {
                        Ok(StreamLine::Finished(Some(content.to_string())))
                    }
                    _ => Ok(StreamLine::Done),
                };
            }
            match content {
                Some(content) if !content.is_empty() => Ok(StreamLine::Delta(content.to_string())),
                _ => Ok(StreamLine::Skip),
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
    if let Ok(value) = serde_json::from_str::<Value>(line) {
        usage_fields(&value, acc);
        parsed.reasoning = value["message"]["thinking"].as_str().map(str::to_string);
        if value["done_reason"] == "length" {
            parsed.stop = Some(StopReason::Length);
        }
    }
    Ok(parsed)
}

#[derive(Default)]
struct OllamaToolParser {
    calls: Vec<ToolCall>,
}
impl crate::streaming::ToolParser for OllamaToolParser {
    fn parse(
        &mut self,
        line: &str,
        usage: &mut UsageAcc,
    ) -> Result<crate::streaming::ToolLine, ProviderError> {
        let event = parse_stream_line(line)?;
        let mut result = crate::streaming::ToolLine {
            end: matches!(&event, StreamLine::Finished(_) | StreamLine::Done),
            parsed: crate::streaming::ParsedLine {
                event,
                reasoning: None,
                stop: None,
            },
            tool_activity: false,
        };
        if let SseField::Data(data) = sse_field(line) {
            let value: Value = serde_json::from_str(data)
                .map_err(|error| ProviderError::Parse(error.to_string()))?;
            result.parsed.reasoning = value["message"]["thinking"].as_str().map(str::to_string);
            if let Some(values) = value.get("message").and_then(tool_call_values) {
                result.tool_activity = true;
                self.calls
                    .extend(parse_tool_calls(values, self.calls.len())?);
            }
            if result.end {
                usage_fields(&value, usage);
                result.parsed.stop = Some(if value["done_reason"] == "length" {
                    StopReason::Length
                } else {
                    StopReason::Complete
                });
            }
        }
        Ok(result)
    }
    fn finish(&mut self) -> Result<Vec<ToolCall>, ProviderError> {
        Ok(std::mem::take(&mut self.calls))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::fake::{FakeHttpClient, FakeState};
    use futures::{StreamExt, executor::block_on};
    use openwebide_core::{ChatMessage, Role, ToolDefinition, TurnTelemetry};

    #[test]
    fn oversized_usage_keeps_telemetry_saturated() {
        let mut acc = UsageAcc::new(&request(None, None));
        usage_fields(
            &json!({"prompt_eval_count": u64::MAX, "eval_count": 1}),
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

    const BASE: &str = "http://localhost:11434";

    fn provider(http: FakeHttpClient) -> (OllamaProvider<FakeHttpClient>, Arc<FakeState>) {
        let state = http.state();
        (
            OllamaProvider::new(BASE, Some("qwen2.5-coder:7b".into()), http),
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
    fn list_models_parses_tags() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "models": [ { "name": "qwen2.5-coder:7b" }, { "name": "llama3.2" } ]
        })));

        let models = block_on(provider.list_models()).unwrap();

        assert_eq!(
            models,
            vec![
                ModelInfo {
                    name: "qwen2.5-coder:7b".into()
                },
                ModelInfo {
                    name: "llama3.2".into()
                },
            ]
        );
        assert_eq!(
            state.calls.lock().unwrap()[0].url,
            format!("{BASE}/api/tags")
        );
    }

    #[test]
    fn list_models_rejects_missing_models() {
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
            "message": { "role": "assistant", "content": "hello" },
            "done": true
        })));

        assert_eq!(
            block_on(provider.chat(&request(None, Some("be brief")))).unwrap(),
            "hello"
        );

        let calls = state.calls.lock().unwrap();
        assert_eq!(calls[0].url, format!("{BASE}/api/chat"));
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["model"], "qwen2.5-coder:7b");
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
    fn chat_prefers_request_model_over_connection() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({ "message": { "content": "ok" } })));

        block_on(provider.chat(&request(Some("other-model"), None))).unwrap();

        let calls = state.calls.lock().unwrap();
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["model"], "other-model");
    }

    #[test]
    fn chat_without_any_model_is_an_error() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = OllamaProvider::new(BASE, None, http);

        assert!(matches!(
            block_on(provider.chat(&request(None, None))),
            Err(ProviderError::NoModel)
        ));
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn http_errors_propagate() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Err(ProviderError::Http(
            "404 Not Found: model 'nope' not found".into(),
        )));

        assert!(matches!(
            block_on(provider.chat(&request(None, None))),
            Err(ProviderError::Http(_))
        ));
    }

    /// Run a stream to completion, splitting deltas from the (at most one)
    /// usage chunk.
    fn run_stream(
        provider: &OllamaProvider<FakeHttpClient>,
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
            r#"{"message":{"role":"assistant","content":"Hel"}}
"#,
            r#"{"message":{"role":"assistant","content":"lo"}}
"#,
            r#"{"message":{"role":"assistant","content":""},"done":false}
"#,
            r#"{"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":12,"eval_count":4,"eval_duration":2000000}
"#,
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["Hel", "lo"]);
        assert!(errors.is_empty());
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].prompt_tokens, 12);
        assert_eq!(usages[0].completion_tokens, 4);
        assert_eq!(usages[0].eval_duration_ms, 2);
        assert!(!usages[0].estimated);
        let calls = state.calls.lock().unwrap();
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn chat_stream_splits_lines_across_chunks() {
        let (provider, state) = provider(FakeHttpClient::new());
        // One chunk holds a partial line, the next finishes it and adds more.
        state.push_stream(vec![
            r#"{"message":{"role":"assistant","content":"a"},"done":fal"#,
            r#"se}
{"message":{"role":"assistant","content":"b"},"done":true}
"#,
        ]);

        let (deltas, _usages, _errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["a", "b"]);
    }

    #[test]
    fn chat_stream_decodes_utf8_split_across_chunks() {
        let (provider, state) = provider(FakeHttpClient::new());
        // "héllo" with the 'é' (0xC3 0xA9) split across chunk boundaries.
        state.push_stream_bytes(vec![
            br#"{"message":{"role":"assistant","content":"h"#.to_vec(),
            vec![0xC3],
            vec![0xA9],
            br#"llo"},"done":true}
"#
            .to_vec(),
        ]);

        let (deltas, _usages, _errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["héllo"]);
    }

    #[test]
    fn chat_tools_empty_tool_calls_is_text() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "message": {
                "role": "assistant",
                "content": "final answer",
                "tool_calls": []
            },
            "done": true
        })));

        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        assert_eq!(
            completion.response,
            ChatResponse::Text("final answer".into())
        );
    }

    #[test]
    fn chat_stream_without_done_is_incomplete() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"{"message":{"role":"assistant","content":"partial"}}
"#,
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));

        assert_eq!(deltas, vec!["partial"]);
        assert!(usages.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ProviderError::Incomplete));
    }

    #[test]
    fn chat_stream_reports_provider_errors() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"{"error":"model 'nope' not found"}
"#,
        ]);

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));
        assert!(deltas.is_empty());
        assert!(usages.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ProviderError::Http(msg) if msg.contains("not found")));
    }

    #[test]
    fn chat_stream_propagates_transport_errors() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream_error(ProviderError::Http("connection reset".into()));

        let (deltas, usages, errors) = run_stream(&provider, &request(None, None));
        assert!(deltas.is_empty());
        assert!(usages.is_empty());
        assert!(matches!(&errors[0], ProviderError::Http(msg) if msg.contains("connection reset")));
        assert_eq!(state.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn chat_stream_without_any_model_is_an_error() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = OllamaProvider::new(BASE, None, http);

        let (_deltas, _usages, errors) = run_stream(&provider, &request(None, None));

        assert!(matches!(errors[0], ProviderError::NoModel));
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn chat_tools_returns_tool_calls() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    { "function": { "name": "read_file", "arguments": { "path": "src/main.rs" } } }
                ]
            },
            "done": true,
            "prompt_eval_count": 20,
            "eval_count": 5,
            "eval_duration": 1_000_000
        })));

        let mut req = request(None, None);
        req.tools = vec![read_file_tool()];
        let completion = block_on(provider.chat_tools(&req)).unwrap();

        assert_eq!(
            completion.response,
            ChatResponse::ToolCalls(vec![ToolCall {
                id: "call_0".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }])
        );
        let usage = completion.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 20);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.eval_duration_ms, 1);
        assert!(!usage.estimated);
        // The request carries the tool definitions and stream is off.
        let calls = state.calls.lock().unwrap();
        let body = calls[0].body.as_ref().unwrap();
        assert_eq!(body["stream"], false);
        assert_eq!(
            body["tools"][0],
            json!({
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a file",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }
                }
            })
        );
    }

    #[test]
    fn chat_tools_returns_text_when_no_calls() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "message": { "role": "assistant", "content": "all done" },
            "done": true
        })));

        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        assert_eq!(completion.response, ChatResponse::Text("all done".into()));
        // No usage fields on the response: the estimator filled them in.
        assert!(completion.usage.unwrap().estimated);
    }

    #[test]
    fn chat_tools_sends_tool_call_and_result_messages() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "message": { "role": "assistant", "content": "fixed it" },
            "done": true
        })));

        let assistant = ChatMessage {
            id: 2,
            session_id: 1,
            role: Role::Assistant,
            content: String::new(),
            created_at: 0,
            tool_calls: Some(vec![ToolCall {
                id: "call_0".into(),
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
            tool_call_id: Some("call_0".into()),
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
        // Assistant message carries tool_calls with object arguments (Ollama
        // wire format) and a null content.
        assert_eq!(
            body["messages"][1],
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    { "function": { "name": "read_file", "arguments": { "path": "src/main.rs" } } }
                ]
            })
        );
        // Tool result carries no tool_call_id (Ollama wire format).
        assert_eq!(
            body["messages"][2],
            json!({ "role": "tool", "content": "fn main() {}" })
        );
    }

    #[test]
    fn chat_tools_without_any_model_is_an_error() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = OllamaProvider::new(BASE, None, http);

        assert!(matches!(
            block_on(provider.chat_tools(&request(None, None))),
            Err(ProviderError::NoModel)
        ));
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn with_num_ctx_adds_options_to_every_request_body() {
        let (provider, state) = provider(FakeHttpClient::new());
        let provider = provider.with_num_ctx(Some(8192));

        state.push(Ok(json!({ "message": { "content": "hi" } })));
        block_on(provider.chat(&request(None, None))).unwrap();
        assert_eq!(
            state.calls.lock().unwrap()[0].body.as_ref().unwrap()["options"]["num_ctx"],
            8192
        );

        state.push_stream(vec![
            r#"{"message":{"content":"a"},"done":true}
"#,
        ]);
        block_on(
            provider
                .chat_stream(&request(None, None))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            state.calls.lock().unwrap()[1].body.as_ref().unwrap()["options"]["num_ctx"],
            8192
        );

        state.push(Ok(json!({ "message": { "content": "hi" } })));
        block_on(provider.chat_tools(&request(None, None))).unwrap();
        assert_eq!(
            state.calls.lock().unwrap()[2].body.as_ref().unwrap()["options"]["num_ctx"],
            8192
        );
    }

    #[test]
    fn without_num_ctx_the_body_has_no_options_key() {
        let (provider, state) = provider(FakeHttpClient::new());

        state.push(Ok(json!({ "message": { "content": "hi" } })));
        block_on(provider.chat(&request(None, None))).unwrap();
        assert!(
            state.calls.lock().unwrap()[0]
                .body
                .as_ref()
                .unwrap()
                .get("options")
                .is_none()
        );
    }

    #[test]
    fn context_limit_reads_num_ctx_from_parameters() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "parameters": "num_ctx 8192\nstop \"<|eot|>\"",
        })));

        let limit = block_on(provider.context_limit(None)).unwrap();
        assert_eq!(limit, Some(8192));
        let calls = state.calls.lock().unwrap();
        assert_eq!(calls[0].url, format!("{BASE}/api/show"));
        assert_eq!(calls[0].body.as_ref().unwrap()["model"], "qwen2.5-coder:7b");
    }

    #[test]
    fn context_limit_ignores_model_info_trained_max() {
        // `model_info.<arch>.context_length` is the trained maximum, not the
        // runtime window Ollama actually loads without a configured
        // `num_ctx`, so it must not be used as a fallback.
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "parameters": "stop \"<|eot|>\"",
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 32768,
            }
        })));
        state.push(Ok(json!({ "models": [] })));

        let limit = block_on(provider.context_limit(Some("qwen2.5-coder:7b"))).unwrap();
        assert_eq!(limit, None);
    }

    #[test]
    fn context_limit_falls_back_to_running_model_ps_window() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "parameters": "stop \"<|eot|>\"",
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 32768,
            }
        })));
        state.push(Ok(json!({
            "models": [
                { "name": "qwen2.5-coder:latest", "context_length": 4096 }
            ]
        })));

        let limit = block_on(provider.context_limit(Some("qwen2.5-coder"))).unwrap();
        assert_eq!(limit, Some(4096));
        let calls = state.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].url, format!("{BASE}/api/show"));
        assert_eq!(calls[1].url, format!("{BASE}/api/ps"));
    }

    #[test]
    fn context_limit_with_no_model_returns_none_without_a_request() {
        let http = FakeHttpClient::new();
        let state = http.state();
        let provider = OllamaProvider::new(BASE, None, http);

        assert_eq!(block_on(provider.context_limit(None)).unwrap(), None);
        assert!(state.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn missing_prompt_eval_count_is_estimated() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push(Ok(json!({
            "message": { "role": "assistant", "content": "hi there" },
            "done": true,
            "eval_count": 3,
            "eval_duration": 1_000_000
        })));

        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        assert!(completion.usage.unwrap().estimated);
    }
    fn tool_items(
        provider: &OllamaProvider<FakeHttpClient>,
        req: &ChatRequest,
    ) -> Vec<Result<ToolStreamChunk, ProviderError>> {
        block_on(provider.chat_tools_stream(req).collect())
    }

    #[test]
    fn tool_stream_emits_content_usage_and_normalized_calls() {
        let (provider, state) = provider(FakeHttpClient::new());
        let provider = provider.with_num_ctx(Some(8192));
        state.push_stream(vec![
            r#"{"message":{"content":"Let "}}
"#,
            r#"{"message":{"content":"me check","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"a"}}}]}}
"#,
            r#"{"message":{"tool_calls":[{"function":{"name":"read_file","arguments":"{}"}}]},"done":true,"prompt_eval_count":40,"eval_count":10,"eval_duration":200000000}
"#,
        ]);
        let mut req = request(None, None);
        req.tools = vec![read_file_tool()];
        let items = tool_items(&provider, &req);
        assert!(matches!(
            items[2],
            Ok(ToolStreamChunk::Stop(StopReason::Complete))
        ));
        let calls = vec![
            ToolCall {
                id: "call_0".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a"}"#.into(),
            },
            ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        ];
        assert_eq!(items.len(), 5);
        assert_eq!(
            items[0].as_ref().unwrap(),
            &ToolStreamChunk::Delta("Let ".into())
        );
        assert_eq!(
            items[1].as_ref().unwrap(),
            &ToolStreamChunk::Delta("me check".into())
        );
        assert_eq!(
            items[3].as_ref().unwrap(),
            &ToolStreamChunk::Usage(TurnTelemetry {
                context: None,
                prompt_tokens: 40,
                completion_tokens: 10,
                eval_duration_ms: 200,
                estimated: false
            })
        );
        assert_eq!(
            items[4].as_ref().unwrap(),
            &ToolStreamChunk::Response(ChatResponse::ToolCalls(calls.clone()))
        );
        let body = state.calls.lock().unwrap()[0].body.clone().unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["options"]["num_ctx"], 8192);
        state.push(Ok(
            json!({"message":{"content":"Let me check","tool_calls":[
                {"function":{"name":"read_file","arguments":{"path":"a"}}},
                {"function":{"name":"read_file","arguments":"{}"}}
            ]}}),
        ));
        let completion = block_on(provider.chat_tools(&req)).unwrap();
        assert_eq!(completion.preamble, "Let me check");
        assert_eq!(completion.response, ChatResponse::ToolCalls(calls));
    }

    #[test]
    fn tool_stream_text_only_concatenates_including_final_delta() {
        let (provider, state) = provider(FakeHttpClient::new());
        state.push_stream(vec![
            r#"{"message":{"content":"a","tool_calls":[]}}
"#,
            r#"{"message":{"content":"b"},"done":true}
"#,
        ]);
        let items = tool_items(&provider, &request(None, None));
        assert!(matches!(
            items[2],
            Ok(ToolStreamChunk::Stop(StopReason::Complete))
        ));
        assert_eq!(items.len(), 5);
        assert_eq!(
            items[0].as_ref().unwrap(),
            &ToolStreamChunk::Delta("a".into())
        );
        assert_eq!(
            items[1].as_ref().unwrap(),
            &ToolStreamChunk::Delta("b".into())
        );
        assert!(matches!(items[3], Ok(ToolStreamChunk::Usage(_))));
        assert_eq!(
            items[4].as_ref().unwrap(),
            &ToolStreamChunk::Response(ChatResponse::Text("ab".into()))
        );
    }

    #[test]
    fn tool_stream_incomplete_and_error_never_emit_usage_or_response() {
        for error in [false, true] {
            let (provider, state) = provider(FakeHttpClient::new());
            let mut lines = vec![
                r#"{"message":{"content":"partial"}}
"#,
            ];
            if error {
                lines.push(
                    r#"{"error":"failed"}
"#,
                );
            }
            state.push_stream(lines);
            let items = tool_items(&provider, &request(None, None));
            assert_eq!(items.len(), 2);
            assert!(matches!(items[0], Ok(ToolStreamChunk::Delta(_))));
            if error {
                assert!(matches!(&items[1], Err(ProviderError::Http(msg)) if msg == "failed"));
            } else {
                assert!(matches!(items[1], Err(ProviderError::Incomplete)));
            }
        }
    }
    #[test]
    fn reasoning_and_stop_reason_stream_in_order() {
        for length in [false, true] {
            for tools in [false, true] {
                let (provider, state) = provider(FakeHttpClient::new());
                state.push_stream(vec![
                    "{\"message\":{\"thinking\":\"r\"}}\n",
                    "{\"message\":{\"content\":\"answer\"}}\n",
                    if length {
                        "{\"done\":true,\"done_reason\":\"length\"}\n"
                    } else {
                        "{\"done\":true,\"done_reason\":\"stop\"}\n"
                    },
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
        state.push(Ok(json!({"message":{"content":"answer","thinking":"r"},"done":true,"done_reason":"length"})));
        let completion = block_on(provider.chat_tools(&request(None, None))).unwrap();
        let chunks = crate::completion_chunks(completion);
        assert_eq!(chunks[0], ToolStreamChunk::Reasoning("r".into()));
        assert_eq!(chunks[1], ToolStreamChunk::Delta("answer".into()));
        assert_eq!(chunks[2], ToolStreamChunk::Stop(StopReason::Length));
        assert!(matches!(chunks[3], ToolStreamChunk::Usage(_)));
    }
}
