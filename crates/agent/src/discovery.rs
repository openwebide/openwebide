//! Run-scoped tool discovery. Only tools already granted to the run are indexed.
use std::collections::{BTreeSet, VecDeque};

use openwebide_core::{ChatRequest, ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;

use crate::ToolOutcome;

const CORE: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "search",
    "grep_search",
    "run_command",
];
const NAME: &str = "tool_search";
const MAX_MATCHES: usize = 3;

pub(crate) struct ToolCatalog {
    optional: Vec<ToolDefinition>,
    active: VecDeque<String>,
    schema_budget: usize,
}

impl ToolCatalog {
    pub(crate) fn new(request: &mut ChatRequest, limit: Option<usize>) -> Self {
        let budget = limit.unwrap_or(usize::MAX) / 10;
        let optional: Vec<_> = request
            .tools
            .iter()
            .filter(|tool| !CORE.contains(&tool.name.as_str()))
            .cloned()
            .collect();
        let mut catalog = Self {
            optional: Vec::new(),
            active: VecDeque::new(),
            schema_budget: limit.unwrap_or(usize::MAX) / 4,
        };
        // Leave small sets and unknown context limits untouched. Avoid shadowing a
        // host tool with the reserved discovery name, or ambiguous duplicate names.
        let unique: BTreeSet<_> = request.tools.iter().map(|tool| &tool.name).collect();
        if limit.is_none()
            || optional.is_empty()
            || unique.len() != request.tools.len()
            || request.tools.iter().any(|tool| tool.name == NAME)
            || openwebide_core::context::tool_schema_tokens(&optional) <= budget
        {
            return catalog;
        }
        let mut tools = request
            .tools
            .iter()
            .filter(|tool| CORE.contains(&tool.name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        tools.push(definition());
        let name_budget = limit.unwrap_or_default().saturating_mul(3) / 20;
        let mut names = String::new();
        let mut shown = 0;
        for tool in &optional {
            if names
                .len()
                .saturating_add(tool.name.len())
                .saturating_add(2)
                > name_budget
            {
                break;
            }
            if shown > 0 {
                names.push_str(", ");
            }
            names.push_str(&tool.name);
            shown += 1;
        }
        if shown < optional.len() {
            names.push_str(&format!("; {} more indexed tools", optional.len() - shown));
        }
        let instructions = format!(
            "\n\nOptional tools: {names}. Use tool_search with keywords or select: followed by exact names. Loaded tools become callable on the next turn."
        );
        if openwebide_core::context::tool_schema_tokens(&tools)
            .saturating_add(instructions.len().div_ceil(3))
            >= openwebide_core::context::tool_schema_tokens(&request.tools)
        {
            return catalog;
        }
        request.tools = tools;
        request
            .system_prompt
            .get_or_insert_with(String::new)
            .push_str(&instructions);
        catalog.optional = optional;
        catalog
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.optional.iter().any(|tool| tool.name == name)
    }

    pub(crate) fn handles(&self, name: &str) -> bool {
        !self.optional.is_empty() && name == NAME
    }

    pub(crate) fn search(&mut self, request: &mut ChatRequest, call: &ToolCall) -> ToolOutcome {
        let result = self.load(request, &call.arguments);
        match result {
            Ok(names) => ToolOutcome {
                ok: true,
                summary: if names.is_empty() { "No matching tools".into() } else { format!("Loaded tools: {}", names.join(", ")) },
                content: json!({"loaded":names,"instruction":"Use loaded tools on the next turn. Search again if a tool is no longer listed."}).to_string(),
                diff: None,
            },
            Err(error) => ToolOutcome { ok: false, content: error.clone(), summary: error, diff: None },
        }
    }

    fn load(&mut self, request: &mut ChatRequest, arguments: &str) -> Result<Vec<String>, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Query {
            query: String,
        }
        let query = serde_json::from_str::<Query>(arguments)
            .map_err(|_| "Provide a query string.".to_string())?
            .query;
        let query = query.trim();
        if query.is_empty() || query.len() > 512 {
            return Err("Query must contain 1–512 bytes.".into());
        }
        let selected = if let Some(names) = query.strip_prefix("select:") {
            let names: BTreeSet<_> = names
                .split([',', ' ', '\n'])
                .filter(|name| !name.is_empty())
                .collect();
            if names.is_empty()
                || names.len() > MAX_MATCHES
                || names
                    .iter()
                    .any(|name| !self.optional.iter().any(|tool| tool.name == *name))
            {
                return Err("Select one to three exact optional tool names from this run.".into());
            }
            self.optional
                .iter()
                .filter(|tool| names.contains(tool.name.as_str()))
                .collect::<Vec<_>>()
        } else {
            let words = query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let mut matches = self
                .optional
                .iter()
                .filter_map(|tool| {
                    let name = tool.name.to_lowercase();
                    let description = tool.description.to_lowercase();
                    let score: usize = words
                        .iter()
                        .map(|word| {
                            if name.contains(word) {
                                3
                            } else {
                                usize::from(description.contains(word))
                            }
                        })
                        .sum();
                    (score > 0).then_some((score, tool))
                })
                .collect::<Vec<_>>();
            matches
                .sort_by(|(a, left), (b, right)| b.cmp(a).then_with(|| left.name.cmp(&right.name)));
            matches
                .into_iter()
                .take(MAX_MATCHES)
                .map(|(_, tool)| tool)
                .collect()
        };
        let names = selected
            .into_iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        for name in names.iter().rev() {
            self.active.retain(|active| active != name);
            self.active.push_back(name.clone());
        }
        loop {
            request
                .tools
                .retain(|tool| CORE.contains(&tool.name.as_str()) || tool.name == NAME);
            request.tools.extend(
                self.active
                    .iter()
                    .filter_map(|name| self.optional.iter().find(|tool| tool.name == *name))
                    .cloned(),
            );
            // Retain at least the most recently selected tool, even when a single
            // schema exceeds this soft share. Normal context budgeting remains final.
            if self.active.len() <= 1
                || openwebide_core::context::tool_schema_tokens(&request.tools)
                    <= self.schema_budget
            {
                break;
            }
            self.active.pop_front();
        }
        Ok(names
            .into_iter()
            .filter(|name| self.active.contains(name))
            .collect())
    }
}

fn definition() -> ToolDefinition {
    ToolDefinition {
        name: NAME.into(),
        description: "Find optional tools by keywords, or select: followed by up to three exact names. Loads their schemas for the next turn.".into(),
        parameters: json!({"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":512}},"required":["query"],"additionalProperties":false}),
    }
}

pub(crate) async fn prepare<
    P: openwebide_llm::LlmProvider,
    S: crate::compaction::CompactionSource,
>(
    provider: &P,
    source: &S,
    request: &mut ChatRequest,
    catalog: &mut Option<ToolCatalog>,
) -> Result<Option<openwebide_core::Compaction>, String> {
    if catalog.is_none() {
        let Some(limit) = crate::compaction::resolve_limit(provider, source, request).await else {
            return Ok(None);
        };
        *catalog = Some(ToolCatalog::new(request, Some(limit)));
    }
    crate::compaction::prepare(provider, source, request).await
}
