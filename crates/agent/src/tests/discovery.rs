use super::*;

fn catalog_request() -> ChatRequest {
    let mut request = request();
    request.model_settings.context_limit = Some(32768);
    request.tools.retain(|tool| {
        matches!(
            tool.name.as_str(),
            "read_file"
                | "write_file"
                | "edit_file"
                | "list_dir"
                | "search"
                | "grep_search"
                | "run_command"
        )
    });
    for name in [
        "mcp_list_issues",
        "mcp_edit_issue",
        "plugin_calendar",
        "plugin_weather",
    ] {
        request.tools.push(ToolDefinition {
            name: name.into(),
            description: format!("{name} {}", "details ".repeat(500)),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        });
    }
    request
}

#[test]
fn deferred_schemas_activate_on_next_turn_and_do_not_reach_host_executor() {
    let (provider, seen) = FakeProvider::new(vec![
        Ok(no_usage(ChatResponse::ToolCalls(vec![
            call(
                "find",
                "tool_search",
                r#"{"query":"select:mcp_list_issues"}"#,
            ),
            call("early", "mcp_list_issues", "{}"),
        ]))),
        Ok(no_usage(ChatResponse::ToolCalls(vec![call(
            "ready",
            "mcp_list_issues",
            "{}",
        )]))),
        Ok(no_usage(ChatResponse::Text("done".into()))),
    ]);
    let executor = FakeExecutor::new(vec![]);
    let executed = executor.executed.clone();
    let events = collect(run(
        provider,
        executor,
        catalog_request(),
        AgentConfig::default(),
        NoopCancel,
        NoopGate,
        1,
    ));
    assert_eq!(*executed.lock().unwrap(), ["mcp_list_issues"]);
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolResult {name,ok:false,..} if name == "mcp_list_issues")));
    let requests = seen.lock().unwrap();
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "read_file")
    );
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "run_command")
    );
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "tool_search")
    );
    assert!(
        !requests[0]
            .tools
            .iter()
            .any(|tool| tool.name.starts_with("mcp_"))
    );
    assert!(
        requests[0]
            .system_prompt
            .as_ref()
            .unwrap()
            .contains("plugin_weather")
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "mcp_list_issues")
    );
    assert!(
        !requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "mcp_edit_issue")
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.role == Role::Tool
                && message.tool_call_id.as_deref() == Some("find")
                && message.content.contains("mcp_list_issues"))
    );
}

#[test]
fn discovered_mutations_keep_approval_and_never_expand_run_authority() {
    struct Deny(Arc<Mutex<Vec<String>>>);
    impl PermissionGate for Deny {
        async fn approve(&self, call: &ToolCall) -> bool {
            self.0.lock().unwrap().push(call.name.clone());
            false
        }
    }
    let (provider, _) = FakeProvider::new(vec![
        Ok(no_usage(ChatResponse::ToolCalls(vec![call(
            "outside",
            "tool_search",
            r#"{"query":"select:ungranted_tool"}"#,
        )]))),
        Ok(no_usage(ChatResponse::ToolCalls(vec![call(
            "find",
            "tool_search",
            r#"{"query":"select:mcp_edit_issue"}"#,
        )]))),
        Ok(no_usage(ChatResponse::ToolCalls(vec![call(
            "edit",
            "mcp_edit_issue",
            "{}",
        )]))),
        Ok(no_usage(ChatResponse::Text("done".into()))),
    ]);
    let executor = FakeExecutor::new(vec![]);
    let executed = executor.executed.clone();
    let approvals = Arc::new(Mutex::new(vec![]));
    let events = collect(run(
        provider,
        executor,
        catalog_request(),
        AgentConfig::default(),
        NoopCancel,
        Deny(approvals.clone()),
        1,
    ));
    assert_eq!(*approvals.lock().unwrap(), ["mcp_edit_issue"]);
    assert!(executed.lock().unwrap().is_empty());
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::ToolResult {name,ok:false,..} if name == "tool_search")
    ));
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::PermissionRequest {name,..} if name == "mcp_edit_issue")
    ));
}

#[test]
fn small_sets_and_unknown_limits_keep_original_schema_contract() {
    for limit in [None, Some(32768)] {
        let mut request = request();
        request.model_settings.context_limit = limit;
        let original = request.tools.clone();
        let (provider, seen) =
            FakeProvider::new(vec![Ok(no_usage(ChatResponse::Text("done".into())))]);
        collect(run(
            provider,
            FakeExecutor::new(vec![]),
            request,
            AgentConfig::default(),
            NoopCancel,
            NoopGate,
            1,
        ));
        assert_eq!(seen.lock().unwrap()[0].tools, original);
    }
}

#[test]
fn catalog_keyword_search_is_bounded_and_rotates_optional_schemas() {
    let mut request = catalog_request();
    request.model_settings.context_limit = Some(8192);
    let mut catalog = crate::discovery::ToolCatalog::new(&mut request, Some(8192));
    let found = catalog.search(
        &mut request,
        &call("query", "tool_search", r#"{"query":"calendar"}"#),
    );
    assert!(found.ok);
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == "plugin_calendar")
    );
    let found = catalog.search(
        &mut request,
        &call(
            "query",
            "tool_search",
            r#"{"query":"select:plugin_weather"}"#,
        ),
    );
    assert!(found.ok);
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == "plugin_weather")
    );
    assert!(
        !request
            .tools
            .iter()
            .any(|tool| tool.name == "plugin_calendar")
    );
    assert!(request.tools.iter().any(|tool| tool.name == "read_file"));
    assert!(
        catalog
            .search(
                &mut request,
                &call("ranked", "tool_search", r#"{"query":"mcp"}"#)
            )
            .ok
    );
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == "mcp_edit_issue")
    );
    assert!(
        !request
            .tools
            .iter()
            .any(|tool| tool.name == "mcp_list_issues")
    );
    let original = request.tools.clone();
    for query in [
        r#"{}"#,
        r#"{"query":""}"#,
        r#"{"query":"weather","extra":true}"#,
        r#"{"query":"select:not_granted"}"#,
    ] {
        assert!(
            !catalog
                .search(&mut request, &call("bad", "tool_search", query))
                .ok
        );
        assert_eq!(request.tools, original);
    }
    assert!(
        catalog
            .search(
                &mut request,
                &call("none", "tool_search", r#"{"query":"no_match"}"#)
            )
            .ok
    );
    assert_eq!(request.tools, original);
}

#[test]
fn discovery_uses_detected_model_limits_and_bounds_name_inventory() {
    struct Source;
    impl crate::model::ModelSource for Source {
        fn available(&self) -> bool {
            true
        }
        async fn context_limit(&self, _: &ChatRequest) -> Option<usize> {
            Some(32768)
        }
    }
    let mut request = catalog_request();
    request.model_settings.context_limit = None;
    let (provider, seen) = FakeProvider::new(vec![Ok(no_usage(ChatResponse::Text("done".into())))]);
    collect(run_with_compaction(
        provider,
        FakeExecutor::new(vec![]),
        request,
        AgentConfig::default(),
        NoopCancel,
        NoopGate,
        1,
        Source,
    ));
    assert!(
        seen.lock().unwrap()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "tool_search")
    );
    let mut request = catalog_request();
    for index in 0..150 {
        request.tools.push(ToolDefinition {
            name: format!("very_long_optional_name_{index}"),
            description: "weather forecast".repeat(50),
            parameters: serde_json::json!({"type":"object"}),
        });
    }
    let mut catalog = crate::discovery::ToolCatalog::new(&mut request, Some(8192));
    assert!(request.system_prompt.as_ref().unwrap().len() < 1600);
    assert!(
        request
            .system_prompt
            .as_ref()
            .unwrap()
            .contains("more indexed tools")
    );
    assert!(
        !request
            .system_prompt
            .as_ref()
            .unwrap()
            .contains("very_long_optional_name_149")
    );
    assert!(
        catalog
            .search(
                &mut request,
                &call(
                    "last",
                    "tool_search",
                    r#"{"query":"very_long_optional_name_149"}"#
                )
            )
            .ok
    );
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == "very_long_optional_name_149")
    );
}

#[test]
fn discovery_consumes_shared_delegation_budget_without_executing_a_host_tool() {
    #[derive(Clone)]
    struct Host;
    impl crate::model::ModelSource for Host {}
    impl CancelCheck for Host {
        async fn check(&self) -> bool {
            false
        }
        async fn cancelled(&self) {
            futures::future::pending::<()>().await;
        }
    }
    impl crate::tasks::scheduler::TaskHost for Host {
        fn now_ms(&self) -> u64 {
            0
        }
        async fn run_child(
            &self,
            _: ChatRequest,
            _: String,
            _: usize,
            _: crate::tasks::budget::TaskBudget,
        ) -> Result<crate::tasks::scheduler::ChildStream, String> {
            unreachable!()
        }
    }
    let (provider, seen) = FakeProvider::new(vec![
        Ok(no_usage(ChatResponse::ToolCalls(vec![call(
            "first",
            "tool_search",
            r#"{"query":"select:plugin_weather"}"#,
        )]))),
        Ok(no_usage(ChatResponse::ToolCalls(vec![call(
            "second",
            "tool_search",
            r#"{"query":"select:plugin_calendar"}"#,
        )]))),
        Ok(no_usage(ChatResponse::Text("done".into()))),
    ]);
    let request = catalog_request();
    let executor = FakeExecutor::new(vec![]);
    let executed = executor.executed.clone();
    let budget = crate::tasks::budget::TaskBudget::new(1);
    let executor = crate::tasks::executor::TaskExecutor::new(
        executor,
        Host,
        request.clone(),
        0,
        budget.clone(),
    );
    #[derive(Clone)]
    struct Transport;
    impl crate::mcp::Transport for Transport {
        async fn invoke(
            &self,
            _: &openwebide_core::mcp::Catalog,
            _: &ToolCall,
        ) -> Result<ToolOutcome, String> {
            unreachable!()
        }
    }
    let executor = crate::mcp::McpTools {
        executor,
        transport: Transport,
        catalog: Arc::new(Default::default()),
    };
    let events = collect(run(
        provider,
        executor,
        request,
        AgentConfig::default(),
        NoopCancel,
        NoopGate,
        1,
    ));
    assert!(budget.reserve_tool().is_err());
    assert!(executed.lock().unwrap().is_empty());
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolResult {name,ok:false,summary,..} if name == "tool_search" && summary.contains("limit"))));
    assert!(
        !seen.lock().unwrap()[2]
            .tools
            .iter()
            .any(|tool| tool.name == "plugin_calendar")
    );
}
