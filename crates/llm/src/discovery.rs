//! Read-only server probes. Detection never overwrites user settings.
use crate::{HttpClient, LlmProvider, ProviderError, registry::Provider, url_for};
use openwebide_core::{Connection, ModelDetection, ProviderKind};
use serde_json::{Value, json};

pub(crate) fn positive(value: Option<&Value>) -> Option<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
}
pub(crate) fn running_model<'a>(running: &'a Value, model: &str) -> Option<&'a Value> {
    running
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| {
            models.iter().find(|entry| {
                entry
                    .get("name")
                    .or_else(|| entry.get("model"))
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| {
                        candidate == model
                            || (!model.contains(':')
                                && candidate.strip_suffix(":latest") == Some(model))
                    })
            })
        })
}
fn root(base: &str) -> &str {
    let base = base
        .split(['?', '#'])
        .next()
        .unwrap_or(base)
        .trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

fn sampling(value: &Value, result: &mut ModelDetection) {
    for name in [
        "temperature",
        "top_p",
        "top_k",
        "min_p",
        "repeat_penalty",
        "seed",
    ] {
        if let Some(value) = value.get(name) {
            let settings = openwebide_core::ModelSettings {
                sampling: std::collections::BTreeMap::from([(name.into(), value.clone())]),
                ..Default::default()
            };
            if settings.validate().is_ok() {
                result.sampling.insert(name.into(), value.clone());
            }
        }
    }
}
fn capability(result: &mut ModelDetection, name: &str, enabled: bool) {
    if enabled && !result.capabilities.iter().any(|value| value == name) {
        result.capabilities.push(name.into());
    }
}

/// Cache read-only HTTP probes within one discovery operation. Never share across credentials.
type ProbeReads = std::collections::HashMap<String, Result<Value, ProviderError>>;

struct ProbeHttp<C> {
    http: std::sync::Arc<C>,
    reads: std::sync::Arc<std::sync::Mutex<ProbeReads>>,
}
impl<C> Clone for ProbeHttp<C> {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            reads: self.reads.clone(),
        }
    }
}
impl<C: HttpClient> HttpClient for ProbeHttp<C> {
    async fn get_json(&self, url: &str) -> Result<Value, ProviderError> {
        let cached = self.reads.lock().unwrap().get(url).cloned();
        if let Some(cached) = cached {
            return cached;
        }
        let result = self.http.get_json(url).await;
        self.reads
            .lock()
            .unwrap()
            .insert(url.into(), result.clone());
        result
    }
    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, ProviderError> {
        self.http.post_json(url, body).await
    }
    fn post_stream(
        &self,
        url: &str,
        body: &Value,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>>
    {
        self.http.post_stream(url, body)
    }
}
pub async fn detect_models<C: HttpClient + 'static>(
    connection: &Connection,
    models: &[openwebide_core::ModelInfo],
    preset: openwebide_core::ServerPreset,
    http: C,
) -> Result<std::collections::BTreeMap<String, Result<ModelDetection, String>>, ProviderError> {
    let http = ProbeHttp {
        http: std::sync::Arc::new(http),
        reads: Default::default(),
    };
    let mut detections = std::collections::BTreeMap::new();
    for model in models {
        let detected = detect_with_preset(connection, &model.name, preset, http.clone()).await;
        if matches!(detected, Err(ProviderError::Authentication)) {
            return Err(ProviderError::Authentication);
        }
        detections.insert(
            model.name.clone(),
            detected.map_err(|error| error.to_string()),
        );
    }
    Ok(detections)
}

pub async fn detect<C: HttpClient + 'static>(
    connection: &Connection,
    model: &str,
    http: C,
) -> Result<ModelDetection, ProviderError> {
    detect_with_preset(connection, model, openwebide_core::ServerPreset::Auto, http).await
}

pub async fn detect_with_preset<C: HttpClient + 'static>(
    connection: &Connection,
    model: &str,
    preset: openwebide_core::ServerPreset,
    http: C,
) -> Result<ModelDetection, ProviderError> {
    use openwebide_core::ServerPreset;
    let mut result = ModelDetection::default();
    let base = root(&connection.base_url);
    if connection.kind == ProviderKind::Ollama {
        let show = http
            .post_json(&url_for(base, "/api/show"), &json!({"model":model}))
            .await?;
        result.capabilities = show
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        result.quantization = show
            .pointer("/details/quantization_level")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(parameters) = show.get("parameters").and_then(Value::as_str) {
            let mut values = serde_json::Map::new();
            for line in parameters.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(name), Some(value)) = (parts.next(), parts.next())
                    && let Ok(value) = serde_json::from_str(value)
                {
                    values.insert(name.into(), value);
                }
            }
            let values = Value::Object(values);
            result.context_limit = positive(values.get("num_ctx"));
            result.max_output_tokens = positive(values.get("num_predict"));
            sampling(&values, &mut result);
        }
        capability(
            &mut result,
            "fim",
            show.get("model_info")
                .and_then(Value::as_object)
                .is_some_and(|info| {
                    info.keys()
                        .any(|key| key.contains("fim_pre_token") || key.contains("fim_prefix"))
                }),
        );
        result.source = "Ollama /api/show".into();
        if let Ok(version) = http.get_json(&url_for(base, "/api/version")).await {
            result.server_version = version
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if let Ok(running) = http.get_json(&url_for(base, "/api/ps")).await {
            let entry = running_model(&running, model);
            result.loaded = Some(entry.is_some());
            if let Some(entry) = entry {
                result.context_limit =
                    positive(entry.get("context_length")).or(result.context_limit);
                result.size_bytes = entry.get("size").and_then(Value::as_u64);
                result.cpu_bytes = result
                    .size_bytes
                    .zip(entry.get("size_vram").and_then(Value::as_u64))
                    .map(|(size, vram)| size.saturating_sub(vram));
                if result.context_limit.is_some() {
                    result.source = "Ollama /api/ps runtime context".into();
                }
            }
        }
        return Ok(result);
    }
    let mut probes = vec![
        "/props",
        "/v1/models",
        "/model/info",
        "/api/v1/models",
        "/get_model_info",
        "/api/extra/true_max_context_length",
    ];
    let preferred = match preset {
        ServerPreset::LiteLlm => "/model/info",
        ServerPreset::OpenRouter | ServerPreset::Vllm => "/v1/models",
        ServerPreset::LmStudio => "/api/v1/models",
        ServerPreset::SgLang => "/get_model_info",
        ServerPreset::KoboldCpp => "/api/extra/true_max_context_length",
        _ => "/props",
    };
    probes.retain(|probe| *probe != preferred);
    probes.insert(0, preferred);
    for probe in probes {
        let value = match http.get_json(&url_for(base, probe)).await {
            Ok(value) => value,
            Err(ProviderError::Authentication) => return Err(ProviderError::Authentication),
            Err(_) => continue,
        };
        match probe {
            "/props" => {
                result.context_limit =
                    positive(value.pointer("/default_generation_settings/n_ctx"))
                        .or_else(|| positive(value.get("n_ctx")));
                if let Some(params) = value.pointer("/default_generation_settings/params") {
                    sampling(params, &mut result);
                    result.max_output_tokens = positive(params.get("n_predict"))
                        .or_else(|| positive(params.get("max_tokens")));
                }
                for (key, name) in [
                    ("supports_tool_calls", "tools"),
                    ("supports_thinking", "thinking"),
                ] {
                    capability(
                        &mut result,
                        name,
                        value
                            .pointer(&format!("/chat_template_caps/{key}"))
                            .and_then(Value::as_bool)
                            == Some(true),
                    );
                }
                capability(
                    &mut result,
                    "completion",
                    value.get("chat_template").is_some(),
                );
                capability(
                    &mut result,
                    "thinking",
                    value
                        .pointer("/chat_template_caps/supports_reasoning_effort")
                        .and_then(Value::as_bool)
                        == Some(true),
                );
                result.loaded = value
                    .get("is_sleeping")
                    .and_then(Value::as_bool)
                    .map(|sleeping| !sleeping);
                if let Ok(models) = http.get_json(&url_for(base, "/v1/models")).await
                    && let Some(entry) =
                        models
                            .get("data")
                            .and_then(Value::as_array)
                            .and_then(|entries| {
                                entries.iter().find(|entry| {
                                    entry.get("id").and_then(Value::as_str) == Some(model)
                                })
                            })
                {
                    result.size_bytes = entry.pointer("/meta/size").and_then(Value::as_u64);
                    result.quantization = entry
                        .pointer("/meta/ftype")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    result.context_limit = result
                        .context_limit
                        .or_else(|| positive(entry.pointer("/meta/n_ctx")));
                }

                result.server_version = value
                    .get("build_info")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                result.tokenizer = Some("llama.cpp /apply-template + /tokenize".into());
                result.source = "llama.cpp /props runtime context".into();
            }
            "/v1/models" => {
                if let Some(entry) =
                    value
                        .get("data")
                        .and_then(Value::as_array)
                        .and_then(|models| {
                            models.iter().find(|entry| {
                                entry.get("id").and_then(Value::as_str) == Some(model)
                            })
                        })
                {
                    result.context_limit = positive(entry.get("max_model_len"))
                        .or_else(|| positive(entry.get("context_length")));
                    result.max_output_tokens =
                        positive(entry.pointer("/top_provider/max_completion_tokens"));
                    if let Some(defaults) = entry.get("default_parameters") {
                        sampling(defaults, &mut result);
                    }
                    if let Some(parameters) =
                        entry.get("supported_parameters").and_then(Value::as_array)
                    {
                        capability(
                            &mut result,
                            "tools",
                            parameters
                                .iter()
                                .any(|value| value.as_str() == Some("tools")),
                        );
                        capability(
                            &mut result,
                            "thinking",
                            parameters
                                .iter()
                                .any(|value| value.as_str() == Some("reasoning")),
                        );
                    }
                    let outputs = entry
                        .pointer("/architecture/output_modalities")
                        .and_then(Value::as_array);
                    capability(
                        &mut result,
                        "completion",
                        outputs.is_some_and(|values| {
                            values.iter().any(|value| value.as_str() == Some("text"))
                        }),
                    );
                    result.tokenizer = entry
                        .pointer("/architecture/tokenizer")
                        .and_then(Value::as_str)
                        .map(|name| format!("{name} (request count estimated)"));
                    result.source = if entry.get("context_length").is_some() {
                        "OpenRouter /models"
                    } else {
                        "OpenAI-compatible /v1/models"
                    }
                    .into();
                }
            }
            "/model/info" => {
                if let Some(entry) =
                    value
                        .get("data")
                        .and_then(Value::as_array)
                        .and_then(|models| {
                            models.iter().find(|entry| {
                                entry.get("model_name").and_then(Value::as_str) == Some(model)
                            })
                        })
                {
                    let info = &entry["model_info"];
                    result.context_limit = positive(info.get("max_input_tokens"));
                    result.max_output_tokens = positive(info.get("max_output_tokens"));
                    for (key, name) in [
                        ("supports_function_calling", "tools"),
                        ("supports_reasoning", "thinking"),
                        ("supports_prompt_caching", "prompt caching"),
                    ] {
                        capability(
                            &mut result,
                            name,
                            info.get(key).and_then(Value::as_bool) == Some(true),
                        );
                    }
                    capability(
                        &mut result,
                        "embedding",
                        info.get("mode").and_then(Value::as_str) == Some("embedding"),
                    );
                    capability(
                        &mut result,
                        "completion",
                        info.get("mode").and_then(Value::as_str) == Some("chat"),
                    );
                    sampling(&entry["litellm_params"], &mut result);
                    result.server_version = value
                        .get("version")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    result.source = "LiteLLM /model/info".into();
                }
            }
            "/api/v1/models" => {
                if let Some(entry) =
                    value
                        .get("models")
                        .and_then(Value::as_array)
                        .and_then(|models| {
                            models.iter().find(|entry| {
                                entry.get("key").and_then(Value::as_str) == Some(model)
                            })
                        })
                {
                    let loaded = entry
                        .get("loaded_instances")
                        .and_then(Value::as_array)
                        .and_then(|instances| instances.first());
                    result.loaded = Some(loaded.is_some());
                    result.context_limit = loaded
                        .and_then(|instance| positive(instance.pointer("/config/context_length")));
                    result.size_bytes = entry.get("size_bytes").and_then(Value::as_u64);
                    result.quantization = entry
                        .pointer("/quantization/name")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    capability(
                        &mut result,
                        "embedding",
                        entry.get("type").and_then(Value::as_str) == Some("embedding"),
                    );
                    capability(
                        &mut result,
                        "completion",
                        entry.get("type").and_then(Value::as_str) == Some("llm"),
                    );
                    for (key, name) in [("trained_for_tool_use", "tools"), ("vision", "vision")] {
                        capability(
                            &mut result,
                            name,
                            entry
                                .pointer(&format!("/capabilities/{key}"))
                                .and_then(Value::as_bool)
                                == Some(true),
                        );
                    }
                    result.source = "LM Studio /api/v1/models loaded context".into();
                }
            }
            "/get_model_info" => {
                result.context_limit = positive(value.get("context_length"));
                result.source = "SGLang /get_model_info".into();
            }
            _ => {
                result.context_limit = positive(value.get("value"));
                result.source = "KoboldCpp runtime context".into();
            }
        }
        if result.context_limit.is_some() || !result.capabilities.is_empty() {
            return Ok(result);
        }
    }
    result.source =
        "No runtime context detected; use the configured value or estimated 4,096 tokens".into();
    Ok(result)
}

pub async fn inspect<C: HttpClient + Clone + 'static>(
    base_url: &str,
    kind: Option<ProviderKind>,
    http: C,
) -> Result<openwebide_core::ServerDiscovery, ProviderError> {
    let kind = match kind {
        Some(kind) => kind,
        None => {
            if http
                .get_json(&url_for(root(base_url), "/api/version"))
                .await
                .ok()
                .is_some_and(|value| value.get("version").and_then(Value::as_str).is_some())
            {
                ProviderKind::Ollama
            } else {
                ProviderKind::LlamaCpp
            }
        }
    };
    let connection = Connection {
        id: 0,
        name: base_url.into(),
        kind,
        base_url: base_url.into(),
        model: None,
        enabled: true,
        context_limit: None,
        tool_stream_unsupported: false,
        tool_stream_revision: 0,
        tool_selection: Default::default(),
    };
    let models = Provider::for_connection(&connection, http)
        .list_models()
        .await?;
    Ok(openwebide_core::ServerDiscovery {
        base_url: base_url.into(),
        kind,
        models,
        detections: Default::default(),
        detection_errors: Default::default(),
    })
}

/// Scan the execution host using the same probes on Spin and native bridges.
pub async fn discover<C: HttpClient + Clone + 'static>(
    http: C,
) -> Vec<openwebide_core::ServerDiscovery> {
    futures::future::join_all(
        [11434, 8080, 1234, 8000, 5001, 5000, 1337]
            .into_iter()
            .map(|port| {
                let http = http.clone();
                async move { inspect(&format!("http://127.0.0.1:{port}"), None, http).await }
            }),
    )
    .await
    .into_iter()
    .filter_map(Result::ok)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct Host;
    impl HttpClient for Host {
        async fn get_json(&self, url: &str) -> Result<Value, ProviderError> {
            match url {
                "http://127.0.0.1:11434/api/version" => Ok(json!({"version":"test"})),
                "http://127.0.0.1:11434/api/tags" => Ok(json!({"models":[{"name":"local-model"}]})),
                "http://127.0.0.1:8080/v1/models" => Ok(json!({"data":[{"id":"host-model"}]})),
                _ => Err(ProviderError::Http("unavailable".into())),
            }
        }
        async fn post_json(&self, _url: &str, _body: &Value) -> Result<Value, ProviderError> {
            Err(ProviderError::NotImplemented("unused".into()))
        }
        fn post_stream(
            &self,
            _url: &str,
            _body: &Value,
        ) -> std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>,
        > {
            Box::pin(futures::stream::empty())
        }
    }
    fn connection(kind: ProviderKind) -> Connection {
        Connection {
            id: 1,
            name: String::new(),
            kind,
            base_url: "http://server/v1".into(),
            model: Some("main".into()),
            enabled: true,
            context_limit: None,
            tool_stream_unsupported: false,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        }
    }
    #[test]
    fn batch_discovery_reads_server_metadata_once_per_request() {
        use crate::fake::FakeHttpClient;
        use openwebide_core::{ModelInfo, ServerPreset};
        futures::executor::block_on(async {
            let http = FakeHttpClient::new();
            let state = http.state();
            state.push(Ok(json!({"data":[
                {"id":"main", "context_length":32768},
                {"id":"fast", "context_length":8192}
            ]})));
            let models = ["main", "fast"].map(|name| ModelInfo { name: name.into() });
            let found = detect_models(
                &connection(ProviderKind::LlamaCpp),
                &models,
                ServerPreset::OpenRouter,
                http,
            )
            .await
            .unwrap();
            assert_eq!(found["main"].as_ref().unwrap().context_limit, Some(32768));
            assert_eq!(found["fast"].as_ref().unwrap().context_limit, Some(8192));
            assert_eq!(state.calls.lock().unwrap().len(), 1);
        });
    }

    #[test]
    fn presets_detect_runtime_limits_sampling_and_capabilities() {
        use crate::fake::FakeHttpClient;
        use openwebide_core::ServerPreset;
        futures::executor::block_on(async {
            for (preset, path, metadata) in [
                (
                    ServerPreset::LiteLlm,
                    "/model/info",
                    json!({"data":[{"model_name":"main","model_info":{"max_input_tokens":32768,"max_output_tokens":2048,"mode":"chat","supports_function_calling":true},"litellm_params":{"temperature":0.4}}]}),
                ),
                (
                    ServerPreset::OpenRouter,
                    "/v1/models",
                    json!({"data":[{"id":"main","context_length":32768,"top_provider":{"max_completion_tokens":2048},"default_parameters":{"temperature":0.4},"supported_parameters":["tools"],"architecture":{"output_modalities":["text"],"tokenizer":"Qwen"}}]}),
                ),
                (
                    ServerPreset::LlamaCpp,
                    "/props",
                    json!({"default_generation_settings":{"n_ctx":32768,"params":{"n_predict":2048,"temperature":0.4}},"chat_template_caps":{"supports_tool_calls":true},"chat_template":"test","build_info":"b123"}),
                ),
            ] {
                let http = FakeHttpClient::new();
                let state = http.state();
                state.push(Ok(metadata));
                let result =
                    detect_with_preset(&connection(ProviderKind::LlamaCpp), "main", preset, http)
                        .await
                        .unwrap();
                assert_eq!(result.context_limit, Some(32768));
                assert_eq!(result.max_output_tokens, Some(2048));
                assert_eq!(result.sampling["temperature"], json!(0.4));
                assert!(result.chat_capable());
                assert!(
                    result
                        .capabilities
                        .iter()
                        .any(|capability| capability == "tools")
                );
                assert_eq!(
                    state.calls.lock().unwrap()[0].url,
                    format!("http://server{path}")
                );
            }
        });
    }
    #[test]
    fn ollama_runtime_context_wins_and_reports_cpu_spill() {
        futures::executor::block_on(async {
            let http = crate::fake::FakeHttpClient::new();
            let state = http.state();
            state.push(Ok(json!({"capabilities":["embedding"],"parameters":"num_ctx 2048\ntemperature 0.7","details":{"quantization_level":"Q4_K_M"},"model_info":{"general.context_length":999999}})));
            state.push(Ok(json!({"version":"1.2.3"})));
            state.push(Ok(json!({"models":[{"name":"main","context_length":8192,"size":1000,"size_vram":750}]})));
            let result = detect(&connection(ProviderKind::Ollama), "main", http)
                .await
                .unwrap();
            assert_eq!(result.context_limit, Some(8192));
            assert_eq!(result.cpu_bytes, Some(250));
            assert_eq!(result.server_version.as_deref(), Some("1.2.3"));
            assert!(!result.chat_capable());
        });
    }
    #[test]
    fn optional_probes_fall_back_but_authentication_stops_detection() {
        futures::executor::block_on(async {
            let http = crate::fake::FakeHttpClient::new();
            let state = http.state();
            for _ in 0..6 {
                state.push(Err(ProviderError::Http("not found".into())));
            }
            let result = detect(&connection(ProviderKind::LlamaCpp), "main", http)
                .await
                .unwrap();
            assert_eq!(result.context_limit, None);
            assert!(result.chat_capable());
            let http = crate::fake::FakeHttpClient::new();
            http.state().push(Err(ProviderError::Authentication));
            assert!(matches!(
                detect(&connection(ProviderKind::LlamaCpp), "main", http).await,
                Err(ProviderError::Authentication)
            ));
        });
    }

    #[test]
    fn host_scan_reuses_provider_probes_and_ignores_unavailable_ports() {
        futures::executor::block_on(async {
            let servers = discover(Host).await;
            assert_eq!(servers.len(), 2);
            assert_eq!(servers[0].kind, ProviderKind::Ollama);
            assert_eq!(servers[0].models[0].name, "local-model");
            assert_eq!(servers[1].kind, ProviderKind::LlamaCpp);
            assert_eq!(servers[1].models[0].name, "host-model");
        });
    }
}
