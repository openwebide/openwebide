//! TUI-driven chat models, telemetry, slash commands, and thinking block parsing.

use serde::{Deserialize, Serialize};

/// Contextual editor information attached to a chat turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorContext {
    pub file_path: String,
    pub cursor_line: usize,
    pub cursor_col: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionContext>,
}

/// Highlighted line range and text from the active editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionContext {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

/// Length of the longest run of consecutive backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

impl EditorContext {
    /// Format the active editor context into an XML turn prelude block for the LLM prompt.
    pub fn format_prompt_injection(&self) -> String {
        let mut out = String::new();
        out.push_str("<active_editor_context>\n");
        out.push_str(&format!(
            "Active file: `{}` (cursor at line {}, col {})\n",
            self.file_path, self.cursor_line, self.cursor_col
        ));

        if let Some(sel) = &self.selection {
            let ext = crate::file_type::extension(&self.file_path).unwrap_or_default();
            let lang = match ext.as_str() {
                "rs" => "rust",
                "ts" => "typescript",
                "js" => "javascript",
                "py" => "python",
                "json" => "json",
                "toml" => "toml",
                "md" => "markdown",
                "html" => "html",
                "css" => "css",
                _ => "",
            };

            // Fence the block with one more backtick than the longest run in
            // the selection, so the text can't close the fence early.
            let fence = "`".repeat(longest_backtick_run(&sel.text).saturating_add(1).max(3));
            out.push_str(&format!(
                "Selected code (lines {}-{}):\n{fence}{lang}\n{}\n{fence}\n",
                sel.start_line,
                sel.end_line,
                sel.text.trim_end()
            ));
        }

        out.push_str("</active_editor_context>\n\n");
        out
    }

    /// Format a concise label for the UI context pill (e.g. `main.rs:42` or `main.rs:40-52 (13 lines)`).
    pub fn pill_label(&self) -> String {
        let filename = self.file_path.rsplit('/').next().unwrap_or(&self.file_path);

        match &self.selection {
            Some(sel) => {
                let count = sel.end_line.saturating_sub(sel.start_line) + 1;
                let line_str = if count == 1 { "line" } else { "lines" };
                format!(
                    "{filename}:{}-{} ({count} {line_str})",
                    sel.start_line, sel.end_line
                )
            }
            None => format!("{filename}:{}", self.cursor_line),
        }
    }
}

/// Real-time telemetry metrics for a single agent turn or LLM completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TurnTelemetry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::ContextBreakdown>,
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
    #[serde(default)]
    pub eval_duration_ms: u64,
    /// `true` when the provider omitted a prompt or completion count and the
    /// estimator filled it in.
    #[serde(default)]
    pub estimated: bool,
}

impl TurnTelemetry {
    /// Tokens generated per second, or `None` when the duration is unknown
    /// (`eval_duration_ms == 0`) or no tokens were generated.
    pub fn tokens_per_second(&self) -> Option<f64> {
        if self.eval_duration_ms == 0 || self.completion_tokens == 0 {
            return None;
        }
        Some(self.completion_tokens as f64 / self.eval_duration_ms as f64 * 1000.0)
    }

    /// The total tokens this turn occupies in the model's context window.
    pub fn context_tokens(&self) -> usize {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

/// Fallback context window size when a connection has none configured and
/// provider discovery finds none either.
pub const DEFAULT_CONTEXT_LIMIT: usize = 4_096;

/// Cumulative session-level telemetry for the statusline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTelemetry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_call: Option<TurnTelemetry>,
    #[serde(default)]
    pub context: Option<crate::ContextBreakdown>,
    pub model: String,
    /// Cumulative prompt tokens across every call this session, for
    /// `/tokens`'s "Input" total.
    ///
    /// After a reload mid-agent-run, only each run's final call is
    /// persisted, so this undercounts the intermediate calls of any
    /// multi-turn run. Accepted: the alternative is persisting per-call
    /// telemetry for turns that never produce a message.
    pub total_prompt_tokens: usize,
    /// Cumulative completion tokens; see [`Self::total_prompt_tokens`].
    pub total_completion_tokens: usize,
    /// The most recent call's `prompt_tokens + completion_tokens`, which is
    /// what the gauge and context-window percentage track (not the
    /// cumulative totals above).
    #[serde(default)]
    pub context_tokens: usize,
    #[serde(default)]
    pub context_estimated: bool,
    #[serde(default)]
    pub speed_estimated: bool,
    #[serde(default)]
    pub totals_estimated: bool,
    pub context_limit: usize,
    /// `true` when `context_limit` is the hard-coded [`DEFAULT_CONTEXT_LIMIT`]
    /// fallback, rather than the connection's configured value or a value
    /// discovered from the provider.
    #[serde(default)]
    pub context_limit_estimated: bool,
    pub current_speed_tps: Option<f64>,
    pub tool_calls_count: usize,
}

impl Default for SessionTelemetry {
    fn default() -> Self {
        Self {
            last_call: None,
            context: None,
            model: "default".into(),
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            context_tokens: 0,
            context_estimated: false,
            speed_estimated: false,
            totals_estimated: false,
            context_limit: DEFAULT_CONTEXT_LIMIT,
            context_limit_estimated: true,
            current_speed_tps: None,
            tool_calls_count: 0,
        }
    }
}

impl SessionTelemetry {
    /// Generation speed shared by the statusline and token report.
    pub fn speed_text(&self) -> String {
        self.current_speed_tps
            .map(|speed| format!("{}{speed:.1} t/s", Self::approx(self.speed_estimated)))
            .unwrap_or_else(|| "-- t/s".into())
    }

    /// Compact context usage in thousands for the statusline.
    pub fn compact_context_tokens(&self) -> String {
        format!(
            "{}{:.1}k",
            Self::approx(self.context_estimated),
            self.context_tokens as f64 / 1000.0
        )
    }

    /// Compact context capacity in thousands for the statusline.
    pub fn compact_context_limit(&self) -> String {
        format!(
            "{}{:.0}k",
            Self::approx(self.context_limit_estimated),
            self.context_limit as f64 / 1000.0
        )
    }

    /// Full session accounting with exact token totals and context counts.
    pub fn tokens_report(&self) -> String {
        let telemetry = self;
        let pct = telemetry.context_percent();
        let bar = telemetry.gauge_bar();
        let totals_approx = SessionTelemetry::approx(telemetry.totals_estimated);
        let context_approx = SessionTelemetry::approx(telemetry.context_estimated);
        let limit_approx = SessionTelemetry::approx(telemetry.context_limit_estimated);
        let speed = telemetry.speed_text();
        let input_tokens = format!("{totals_approx}{}", telemetry.total_prompt_tokens);
        let output_tokens = format!("{totals_approx}{}", telemetry.total_completion_tokens);
        let context_tokens = format!("{context_approx}{}", telemetry.context_tokens);
        let context_limit = format!("{limit_approx}{}", telemetry.context_limit);

        format!(
            "```text\n\
         ┌─ Session Token Accounting ────────────────────────────────────┐\n\
         │ Model:             {:<42} │\n\
         │ Input Tokens:      {:<42} │\n\
         │ Output Tokens:     {:<42} │\n\
         │ Context:           {} / {} ({:.1}%) {} │\n\
         │ Current Speed:     {:<42} │\n\
         │ Tool Calls:        {:<42} │\n\
         └───────────────────────────────────────────────────────────────┘\n```",
            telemetry.model,
            input_tokens,
            output_tokens,
            context_tokens,
            context_limit,
            pct,
            bar,
            speed,
            telemetry.tool_calls_count,
        )
    }

    /// Calculate current context window utilization percentage (0.0 to 100.0).
    pub fn context_percent(&self) -> f64 {
        if self.context_limit == 0 {
            return 0.0;
        }
        ((self.context_tokens as f64 / self.context_limit as f64) * 100.0).clamp(0.0, 100.0)
    }

    /// Render a 10-character gauge bar (e.g. `[====······]`).
    pub fn gauge_bar(&self) -> String {
        let pct = self.context_percent();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // context_percent clamps to [0, 100], so rounded cells fit usize.
        let filled = ((pct / 10.0).round() as usize).min(10);
        let empty = 10 - filled;
        format!("[{}{}]", "=".repeat(filled), "·".repeat(empty))
    }

    /// Record one turn's usage: the gauge tracks the latest call, while the
    /// `/tokens` totals accumulate.
    pub fn record_turn(&mut self, t: &TurnTelemetry) {
        self.last_call = Some(*t);
        self.context = t.context;
        self.context_tokens = t.context_tokens();
        self.context_estimated = t.estimated;
        self.total_prompt_tokens = self.total_prompt_tokens.saturating_add(t.prompt_tokens);
        self.total_completion_tokens = self
            .total_completion_tokens
            .saturating_add(t.completion_tokens);
        self.totals_estimated |= t.estimated;
        if let Some(tps) = t.tokens_per_second() {
            self.current_speed_tps = Some(tps);
            self.speed_estimated = t.estimated;
        }
    }

    /// Rebuild the session's telemetry from a reloaded conversation: replay
    /// every message's persisted usage, in order, so the gauge and totals
    /// match what a live session would show.
    ///
    /// After a reload mid-agent-run, only each run's final call was
    /// persisted, so `/tokens`'s totals undercount the run's intermediate
    /// calls. Accepted (see [`Self::total_prompt_tokens`]).
    pub fn restore_from_conversation(&mut self, entries: &[crate::ConversationEntry]) {
        self.last_call = None;
        self.context = None;
        self.context_tokens = 0;
        self.total_prompt_tokens = 0;
        self.total_completion_tokens = 0;
        self.tool_calls_count = 0;
        self.context_estimated = false;
        self.speed_estimated = false;
        self.totals_estimated = false;
        self.current_speed_tps = None;

        let mut any_usage = false;
        for entry in entries {
            match entry {
                crate::ConversationEntry::Message(msg) => {
                    if let Some(usage) = &msg.usage {
                        any_usage = true;
                        self.record_turn(usage);
                    }
                }
                crate::ConversationEntry::Task(task) => {
                    for usage in task.snapshot.usages() {
                        self.total_prompt_tokens =
                            self.total_prompt_tokens.saturating_add(usage.prompt_tokens);
                        self.total_completion_tokens = self
                            .total_completion_tokens
                            .saturating_add(usage.completion_tokens);
                        self.totals_estimated |= usage.estimated;
                    }
                }
                crate::ConversationEntry::ToolStep(_) => {
                    self.tool_calls_count += 1;
                }
            }
        }

        if !any_usage && !entries.is_empty() {
            let (p, c, _) = calculate_conversation_telemetry(entries);
            self.context_tokens = p + c;
            self.context_estimated = true;
        }
    }

    /// The `~` prefix shown before an estimated value, or the empty string.
    pub fn approx(estimated: bool) -> &'static str {
        if estimated { "~" } else { "" }
    }
}

/// Built-in slash commands that can be invoked from the prompt composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help(Option<String>),
    Model(Option<String>),
    Compact,
    Goal(Option<String>),
    Clear,
    Diff(Option<String>),
    Test(Option<String>),
    Tokens,
    Context,
    Stop,
    Commit(Option<String>),
    Checkout(Option<String>),
    Branch(Option<String>),
    Sync,
}

impl SlashCommand {
    /// Parse an input string into a recognized slash command if it starts with `/`.
    pub fn parse(input: &str) -> Option<SlashCommand> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let (cmd, args) = match trimmed.split_once(char::is_whitespace) {
            Some((c, a)) => {
                let arg = a.trim();
                let arg_opt = if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_string())
                };
                (c, arg_opt)
            }
            None => (trimmed, None),
        };

        match cmd {
            "/help" => Some(SlashCommand::Help(args)),
            "/model" => Some(SlashCommand::Model(args)),
            "/compact" => Some(SlashCommand::Compact),
            "/goal" => Some(SlashCommand::Goal(args)),
            "/clear" => Some(SlashCommand::Clear),
            "/diff" => Some(SlashCommand::Diff(args)),
            "/test" => Some(SlashCommand::Test(args)),
            "/tokens" => Some(SlashCommand::Tokens),
            "/context" => Some(SlashCommand::Context),
            "/stop" => Some(SlashCommand::Stop),
            "/commit" => Some(SlashCommand::Commit(args)),
            "/checkout" => Some(SlashCommand::Checkout(args)),
            "/branch" => Some(SlashCommand::Branch(args)),
            "/sync" => Some(SlashCommand::Sync),
            _ => None,
        }
    }
}

/// Shared command discovery for chat clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashInfo {
    pub command: &'static str,
    pub arguments: &'static str,
    pub description: &'static str,
}

pub const SLASH_COMMANDS: &[SlashInfo] = &[
    SlashInfo {
        command: "/help",
        arguments: "[search]",
        description: "Search commands and show keyboard shortcuts",
    },
    SlashInfo {
        command: "/model",
        arguments: "[name|default]",
        description: "List or switch models",
    },
    SlashInfo {
        command: "/clear",
        arguments: "",
        description: "Clear the visible chat stream",
    },
    SlashInfo {
        command: "/compact",
        arguments: "",
        description: "Summarize saved conversation context",
    },
    SlashInfo {
        command: "/goal",
        arguments: "[objective|status|pause|resume|complete]",
        description: "Start and manage a saved objective",
    },
    SlashInfo {
        command: "/diff",
        arguments: "[path]",
        description: "View Git changes or pending edits",
    },
    SlashInfo {
        command: "/commit",
        arguments: "<message>",
        description: "Stage and commit Git changes",
    },
    SlashInfo {
        command: "/checkout",
        arguments: "<branch>",
        description: "Switch Git branch",
    },
    SlashInfo {
        command: "/branch",
        arguments: "[name]",
        description: "List branches or create a branch",
    },
    SlashInfo {
        command: "/sync",
        arguments: "",
        description: "Pull and push upstream commits",
    },
    SlashInfo {
        command: "/test",
        arguments: "[filter]",
        description: "Run detected project tests",
    },
    SlashInfo {
        command: "/tokens",
        arguments: "",
        description: "Show token accounting",
    },
    SlashInfo {
        command: "/context",
        arguments: "",
        description: "Show the latest model input breakdown",
    },
    SlashInfo {
        command: "/stop",
        arguments: "",
        description: "Stop the current run",
    },
];

pub fn slash_search(query: &str) -> Vec<SlashInfo> {
    let query = query.trim().to_ascii_lowercase();
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|info| {
            format!("{} {} {}", info.command, info.arguments, info.description)
                .to_ascii_lowercase()
                .contains(&query)
        })
        .collect()
}

/// Only complete a command name, never replace its arguments or an ordinary prompt.
pub fn slash_suggestions(draft: &str) -> Vec<SlashInfo> {
    if !draft.starts_with('/') || draft.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|info| info.command.starts_with(draft))
        .collect()
}

/// Parsed thinking / reasoning stream extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedThinking {
    /// Extracted chain-of-thought content from inside `<think>...</think>`.
    pub thinking: Option<String>,
    /// Whether `<think>` is currently open and has not yet been closed.
    pub is_thinking: bool,
    /// Final answer or remaining text outside `<think>...</think>`.
    pub answer: String,
}

impl ParsedThinking {
    /// Live output can start with an incomplete reasoning delimiter.
    pub fn has_visible_output(&self) -> bool {
        let answer = self.answer.trim();
        self.thinking.as_ref().is_some_and(|text| !text.is_empty())
            || (!answer.is_empty()
                && !(self.thinking.is_none()
                    && ["<think>", crate::ESCAPED_REASONING_OPEN]
                        .iter()
                        .any(|tag| tag.starts_with(answer))))
    }
}

/// Timing of an observed reasoning phase. The host supplies milliseconds so
/// browser and native clocks share the same transition rules.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReasoningTiming {
    started_ms: Option<f64>,
    elapsed_ms: Option<f64>,
}

impl ReasoningTiming {
    /// Repeated chunks do not restart the clock. A completed historical trace
    /// has no observed duration; do not invent one from its generation usage.
    pub fn observe(&mut self, active: bool, now_ms: f64) {
        if !now_ms.is_finite() {
            return;
        }
        if active {
            self.started_ms.get_or_insert(now_ms);
            self.elapsed_ms.get_or_insert(0.0);
        } else if let Some(started) = self.started_ms.take() {
            self.elapsed_ms =
                Some(self.elapsed_ms.unwrap_or_default() + (now_ms - started).max(0.0));
        }
    }

    pub fn elapsed_ms(self, now_ms: f64) -> Option<f64> {
        self.elapsed_ms.map(|elapsed| {
            elapsed
                + self.started_ms.map_or(0.0, |start| {
                    if now_ms.is_finite() {
                        (now_ms - start).max(0.0)
                    } else {
                        0.0
                    }
                })
        })
    }
}

/// Reasoning counts are estimates: providers report whole-generation usage,
/// which cannot safely be attributed to the reasoning portion alone.
pub fn reasoning_summary(active: bool, elapsed_ms: Option<f64>, tokens: usize) -> String {
    let tokens = if tokens >= 1000 {
        #[allow(clippy::cast_precision_loss)] // Display precision is intentionally one decimal.
        let thousands = tokens as f64 / 1000.0;
        format!("~{thousands:.1}k tokens")
    } else {
        format!("~{tokens} tokens")
    };
    let label = if active { "Thinking" } else { "Thought" };
    match elapsed_ms.filter(|elapsed| elapsed.is_finite()) {
        Some(elapsed) => format!("{label} for {:.1}s · {tokens}", elapsed.max(0.0) / 1000.0),
        None => format!("{label} · {tokens}"),
    }
}

/// Extract thinking content (`<think> ... </think>`) from an assistant text stream.
pub fn parse_thinking(raw: &str) -> ParsedThinking {
    const OPEN_TAG: &str = "<think>";
    const CLOSE_TAG: &str = "</think>";

    let opening = [OPEN_TAG, crate::ESCAPED_REASONING_OPEN]
        .into_iter()
        .filter_map(|tag| raw.find(tag).map(|idx| (idx, tag)))
        .min_by_key(|(idx, _)| *idx);
    if let Some((open_idx, tag)) = opening {
        let decode = |text: &str| {
            if tag == crate::ESCAPED_REASONING_OPEN {
                text.replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&")
            } else {
                text.to_string()
            }
        };
        let prefix = &raw[..open_idx];
        let after_open = &raw[open_idx + tag.len()..];

        if let Some(close_idx) = after_open.find(CLOSE_TAG) {
            let thinking_content = &after_open[..close_idx];
            let after_close = &after_open[close_idx + CLOSE_TAG.len()..];

            let answer = format!("{}{}", prefix, after_close.trim_start_matches('\n'));
            ParsedThinking {
                thinking: Some(decode(thinking_content.trim())),
                is_thinking: false,
                answer,
            }
        } else {
            // Still in progress: tag opened, not yet closed
            ParsedThinking {
                thinking: Some(decode(after_open.trim_start())),
                is_thinking: true,
                answer: prefix.to_string(),
            }
        }
    } else {
        ParsedThinking {
            thinking: None,
            is_thinking: false,
            answer: raw.to_string(),
        }
    }
}

/// Extract any prepended `<active_editor_context>` from a user message content,
/// returning `(Option<extracted_context_string>, clean_user_prompt)`.
pub fn extract_editor_context_prelude(content: &str) -> (Option<&str>, &str) {
    if let Some(rest) = content.strip_prefix("<active_editor_context>\n") {
        if let Some((context_block, prompt)) = rest.split_once("</active_editor_context>\n\n") {
            return (Some(context_block), prompt);
        }
        if let Some((context_block, prompt)) = rest.split_once("</active_editor_context>\n") {
            return (Some(context_block), prompt);
        }
    }
    (None, content)
}

/// Heuristic token estimator for arbitrary text (prose or source code).
///
/// Returns an estimated token count without depending on heavy BPE tokenizers.
/// Combines word-boundary counts and character lengths to remain accurate across
/// natural languages and punctuation-dense code.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    let by_words = (words * 4).div_ceil(3);
    let by_chars = chars.div_ceil(4);
    by_words.max(by_chars).max(1)
}

/// Estimate total input tokens for a [`crate::ChatRequest`] including envelopes and tools.
pub fn estimate_chat_request_tokens(request: &crate::ChatRequest) -> usize {
    let mut tokens = 0;
    if let Some(sys) = &request.system_prompt {
        tokens += estimate_tokens(sys) + 4;
    }
    for msg in &request.messages {
        let prompt = if msg.role == crate::Role::User {
            crate::PromptContent::decode(&msg.content)
        } else {
            crate::PromptContent {
                text: msg.content.clone(),
                ..Default::default()
            }
        };
        tokens += estimate_tokens(&prompt.model_text()) + 4 + prompt.image_tokens();
        if let Some(calls) = &msg.tool_calls {
            for call in calls {
                tokens += estimate_tokens(&call.name) + estimate_tokens(&call.arguments) + 6;
            }
        }
    }
    for tool in &request.tools {
        tokens += estimate_tokens(&tool.name) + estimate_tokens(&tool.description) + 12;
    }
    tokens
}

/// Compute cumulative token statistics and tool executions from a conversation's history.
///
/// Returns `(total_prompt_tokens, total_completion_tokens, tool_calls_count)`.
pub fn calculate_conversation_telemetry(
    entries: &[crate::ConversationEntry],
) -> (usize, usize, usize) {
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;
    let mut tool_calls = 0;

    for entry in entries {
        match entry {
            crate::ConversationEntry::Message(msg) => {
                let count = estimate_tokens(&msg.content) + 4;
                match msg.role {
                    crate::Role::User | crate::Role::System => {
                        prompt_tokens += count;
                    }
                    crate::Role::Assistant => {
                        completion_tokens += count;
                        if let Some(calls) = &msg.tool_calls {
                            tool_calls += calls.len();
                            for c in calls {
                                completion_tokens +=
                                    estimate_tokens(&c.name) + estimate_tokens(&c.arguments);
                            }
                        }
                    }
                    crate::Role::Tool => {
                        prompt_tokens += count;
                        tool_calls += 1;
                    }
                }
            }
            crate::ConversationEntry::Task(task) => {
                for usage in task.snapshot.usages() {
                    prompt_tokens += usage.prompt_tokens;
                    completion_tokens += usage.completion_tokens;
                }
            }
            crate::ConversationEntry::ToolStep(ts) => {
                tool_calls += 1;
                prompt_tokens += estimate_tokens(&ts.summary) + 4;
                if let Some(res) = &ts.result_summary {
                    prompt_tokens += estimate_tokens(res);
                }
            }
        }
    }

    (prompt_tokens, completion_tokens, tool_calls)
}

#[cfg(test)]
mod tests {
    #[test]
    fn commands_share_search_completion_and_whitespace_parsing() {
        assert_eq!(
            SlashCommand::parse("/help compact"),
            Some(SlashCommand::Help(Some("compact".into())))
        );
        assert_eq!(
            SlashCommand::parse("/model\tfast"),
            Some(SlashCommand::Model(Some("fast".into())))
        );
        assert_eq!(SlashCommand::parse("/compact"), Some(SlashCommand::Compact));
        assert_eq!(
            SlashCommand::parse("/goal fix tests"),
            Some(SlashCommand::Goal(Some("fix tests".into())))
        );
        assert_eq!(slash_suggestions("/comp").len(), 1);
        assert!(slash_suggestions("normal /comp").is_empty());
        assert!(slash_suggestions("/goal fix tests").is_empty());
        assert_eq!(slash_search("summarize")[0].command, "/compact");
        for info in SLASH_COMMANDS {
            assert!(SlashCommand::parse(info.command).is_some());
        }
    }

    #[test]
    fn reasoning_timing_survives_chunks_and_freezes_on_completion() {
        let mut timing = super::ReasoningTiming::default();
        timing.observe(false, 100.0);
        assert_eq!(timing.elapsed_ms(200.0), None);
        timing.observe(true, 1000.0);
        timing.observe(true, 2000.0);
        assert_eq!(timing.elapsed_ms(4200.0), Some(3200.0));
        timing.observe(false, 4200.0);
        assert_eq!(timing.elapsed_ms(9000.0), Some(3200.0));
        timing.observe(false, 10000.0);
        assert_eq!(timing.elapsed_ms(11000.0), Some(3200.0));
        timing.observe(true, 12000.0);
        timing.observe(false, 12500.0);
        assert_eq!(timing.elapsed_ms(15000.0), Some(3700.0));
        assert_eq!(
            super::reasoning_summary(false, Some(3200.0), 1400),
            "Thought for 3.2s · ~1.4k tokens"
        );
        assert_eq!(
            super::reasoning_summary(true, Some(200.0), 2),
            "Thinking for 0.2s · ~2 tokens"
        );
        assert_eq!(
            super::reasoning_summary(false, None, 2),
            "Thought · ~2 tokens"
        );
    }

    #[test]
    fn reasoning_clock_rejects_invalid_and_backward_time() {
        let mut timing = super::ReasoningTiming::default();
        timing.observe(true, f64::NAN);
        assert_eq!(timing.elapsed_ms(1.0), None);
        timing.observe(true, 100.0);
        assert_eq!(timing.elapsed_ms(50.0), Some(0.0));
        assert_eq!(timing.elapsed_ms(f64::INFINITY), Some(0.0));
        timing.observe(false, 50.0);
        assert_eq!(timing.elapsed_ms(500.0), Some(0.0));
    }
    use super::*;
    use crate::{ChatMessage, ConversationEntry, Role, ToolStep};

    #[test]
    fn telemetry_text_preserves_precision_estimates_and_missing_speed() {
        let mut telemetry = SessionTelemetry {
            context: None,
            model: "café".into(),
            context_tokens: 12345,
            context_limit: 65536,
            total_prompt_tokens: 12345,
            total_completion_tokens: 42,
            current_speed_tps: Some(12.345),
            context_estimated: true,
            context_limit_estimated: true,
            totals_estimated: true,
            speed_estimated: true,
            ..SessionTelemetry::default()
        };
        assert_eq!(telemetry.compact_context_tokens(), "~12.3k");
        assert_eq!(telemetry.compact_context_limit(), "~66k");
        assert_eq!(telemetry.speed_text(), "~12.3 t/s");
        let report = telemetry.tokens_report();
        assert!(report.contains("│ Context:           ~12345 / ~65536 (18.8%) [==········] │"));
        assert!(report.contains("│ Input Tokens:      ~12345"));
        assert!(report.contains("│ Output Tokens:     ~42"));
        telemetry.context_estimated = false;
        telemetry.context_limit_estimated = false;
        telemetry.speed_estimated = false;
        telemetry.totals_estimated = false;
        assert_eq!(telemetry.compact_context_tokens(), "12.3k");
        assert_eq!(telemetry.compact_context_limit(), "66k");
        assert_eq!(telemetry.speed_text(), "12.3 t/s");
        assert!(!telemetry.tokens_report().contains('~'));
        telemetry.context_tokens = 0;
        telemetry.context_limit = 0;
        telemetry.current_speed_tps = None;
        assert_eq!(telemetry.compact_context_tokens(), "0.0k");
        assert_eq!(telemetry.compact_context_limit(), "0k");
        assert_eq!(telemetry.speed_text(), "-- t/s");
        assert!(
            telemetry
                .tokens_report()
                .contains("0 / 0 (0.0%) [··········]")
        );
    }

    #[test]
    fn test_editor_context_format() {
        let ctx = EditorContext {
            file_path: "crates/core/src/tui.rs".into(),
            cursor_line: 10,
            cursor_col: 4,
            selection: Some(SelectionContext {
                start_line: 10,
                end_line: 12,
                text: "pub struct Test;\npub fn run() {}".into(),
            }),
        };

        let formatted = ctx.format_prompt_injection();
        assert!(formatted.starts_with("<active_editor_context>"));
        assert!(
            formatted.contains("Active file: `crates/core/src/tui.rs` (cursor at line 10, col 4)")
        );
        assert!(formatted.contains("Selected code (lines 10-12):"));
        assert!(formatted.contains("```rust"));
        assert!(formatted.ends_with("</active_editor_context>\n\n"));
        assert_eq!(ctx.pill_label(), "tui.rs:10-12 (3 lines)");
    }

    #[test]
    fn test_prompt_injection_fence_outlives_selection_backticks() {
        let ctx = EditorContext {
            file_path: "README.md".into(),
            cursor_line: 1,
            cursor_col: 1,
            selection: Some(SelectionContext {
                start_line: 1,
                end_line: 3,
                text: "intro\n```\ncode\n```".into(),
            }),
        };

        let formatted = ctx.format_prompt_injection();
        // The selection contains a triple-backtick run, so the fence must be
        // four backticks to stay open.
        assert!(formatted.contains("````markdown\n"));
        assert!(formatted.contains("````\n</active_editor_context>"));
    }

    #[test]
    fn test_slash_commands_parse() {
        assert_eq!(SlashCommand::parse("/help"), Some(SlashCommand::Help(None)));
        assert_eq!(
            SlashCommand::parse("/model deepseek-r1:8b"),
            Some(SlashCommand::Model(Some("deepseek-r1:8b".into())))
        );
        assert_eq!(
            SlashCommand::parse("/model"),
            Some(SlashCommand::Model(None))
        );
        assert_eq!(SlashCommand::parse("/clear"), Some(SlashCommand::Clear));
        assert_eq!(SlashCommand::parse("/tokens"), Some(SlashCommand::Tokens));
        assert_eq!(SlashCommand::parse("/context"), Some(SlashCommand::Context));
        assert_eq!(
            SlashCommand::parse("/test openwebide-auth"),
            Some(SlashCommand::Test(Some("openwebide-auth".into())))
        );
        assert_eq!(
            SlashCommand::parse("/commit feat: initial commit"),
            Some(SlashCommand::Commit(Some("feat: initial commit".into())))
        );
        assert_eq!(
            SlashCommand::parse("/checkout main"),
            Some(SlashCommand::Checkout(Some("main".into())))
        );
        assert_eq!(
            SlashCommand::parse("/branch feat/new-feature"),
            Some(SlashCommand::Branch(Some("feat/new-feature".into())))
        );
        assert_eq!(SlashCommand::parse("/sync"), Some(SlashCommand::Sync));
        assert_eq!(SlashCommand::parse("/unknown"), None);
        assert_eq!(SlashCommand::parse("just a regular message /help"), None);
    }

    #[test]
    fn test_parse_thinking_completed() {
        let raw =
            "<think>\nThinking about the problem...\nDone thinking.\n</think>\nHere is the answer!";
        let parsed = parse_thinking(raw);
        assert_eq!(
            parsed.thinking,
            Some("Thinking about the problem...\nDone thinking.".into())
        );
        assert!(!parsed.is_thinking);
        assert_eq!(parsed.answer, "Here is the answer!");
    }

    #[test]
    fn test_parse_thinking_in_progress() {
        let raw = "<think>\nStill reasoning through step 2...";
        let parsed = parse_thinking(raw);
        assert_eq!(
            parsed.thinking,
            Some("Still reasoning through step 2...".into())
        );
        assert!(parsed.is_thinking);
        assert_eq!(parsed.answer, "");
    }

    #[test]
    fn test_parse_thinking_none() {
        let raw = "Direct response without any thinking block.";
        let parsed = parse_thinking(raw);
        assert_eq!(parsed.thinking, None);
        assert!(!parsed.is_thinking);
        assert_eq!(parsed.answer, raw);
    }

    #[test]
    fn test_session_telemetry_gauge() {
        let telem = SessionTelemetry {
            last_call: None,
            context: None,
            model: "qwen2.5-coder:7b".into(),
            total_prompt_tokens: 16_384,
            total_completion_tokens: 0,
            context_tokens: 16_384,
            context_estimated: false,
            speed_estimated: false,
            totals_estimated: false,
            context_limit: 32_768,
            context_limit_estimated: false,
            current_speed_tps: Some(42.5),
            tool_calls_count: 2,
        };

        assert!((telem.context_percent() - 50.0).abs() < f64::EPSILON);
        assert_eq!(telem.gauge_bar(), "[=====·····]");
    }

    #[test]
    fn test_default_context_limit_is_marked_estimated() {
        // The fallback default (no configured limit, no discovered runtime
        // value) must render with `~`, since it is a guess, not what the
        // provider is actually running with.
        let telem = SessionTelemetry::default();
        assert_eq!(telem.context_limit, DEFAULT_CONTEXT_LIMIT);
        assert!(telem.context_limit_estimated);
        assert_eq!(SessionTelemetry::approx(telem.context_limit_estimated), "~");

        let mut resolved = telem.clone();
        resolved.context_limit_estimated = false;
        assert_eq!(
            SessionTelemetry::approx(resolved.context_limit_estimated),
            ""
        );
    }

    #[test]
    fn test_tokens_per_second() {
        let t = TurnTelemetry {
            context: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            eval_duration_ms: 2_000,
            estimated: false,
        };
        assert_eq!(t.tokens_per_second(), Some(25.0));

        let zero_duration = TurnTelemetry {
            eval_duration_ms: 0,
            completion_tokens: 50,
            ..Default::default()
        };
        assert_eq!(zero_duration.tokens_per_second(), None);

        let zero_tokens = TurnTelemetry {
            eval_duration_ms: 1_000,
            completion_tokens: 0,
            ..Default::default()
        };
        assert_eq!(zero_tokens.tokens_per_second(), None);
    }

    #[test]
    fn test_record_turn_tracks_latest_context_and_accumulates_totals() {
        let mut telem = SessionTelemetry::default();
        telem.record_turn(&TurnTelemetry {
            context: None,
            prompt_tokens: 100,
            completion_tokens: 20,
            eval_duration_ms: 1_000,
            estimated: false,
        });
        assert_eq!(telem.context_tokens, 120);
        assert_eq!(telem.total_prompt_tokens, 100);
        assert_eq!(telem.total_completion_tokens, 20);

        telem.record_turn(&TurnTelemetry {
            context: None,
            prompt_tokens: 130,
            completion_tokens: 10,
            eval_duration_ms: 500,
            estimated: false,
        });
        // The gauge tracks only the latest call...
        assert_eq!(telem.context_tokens, 140);
        // ...while the totals accumulate across every call.
        assert_eq!(telem.total_prompt_tokens, 230);
        assert_eq!(telem.total_completion_tokens, 30);
    }

    #[test]
    fn record_turn_saturates_context_and_cumulative_tokens() {
        let mut telemetry = SessionTelemetry::default();
        let usage = TurnTelemetry {
            context: None,
            prompt_tokens: usize::MAX,
            completion_tokens: 1,
            ..TurnTelemetry::default()
        };
        telemetry.record_turn(&usage);
        assert_eq!(telemetry.context_tokens, usize::MAX);
        assert_eq!(telemetry.gauge_bar(), "[==========]");
        telemetry.record_turn(&TurnTelemetry {
            context: None,
            prompt_tokens: 1,
            completion_tokens: usize::MAX,
            ..TurnTelemetry::default()
        });
        assert_eq!(telemetry.context_tokens, usize::MAX);
        assert_eq!(telemetry.total_prompt_tokens, usize::MAX);
        assert_eq!(telemetry.total_completion_tokens, usize::MAX);
    }

    #[test]
    fn test_record_turn_estimated_flags() {
        let mut telem = SessionTelemetry::default();
        telem.record_turn(&TurnTelemetry {
            context: None,
            prompt_tokens: 0,
            completion_tokens: 10,
            eval_duration_ms: 100,
            estimated: true,
        });
        assert!(telem.context_estimated);
        assert!(telem.speed_estimated);
        assert!(telem.totals_estimated);

        // A following exact turn clears the per-call flags but the
        // cumulative totals-estimated flag stays set.
        telem.record_turn(&TurnTelemetry {
            context: None,
            prompt_tokens: 50,
            completion_tokens: 10,
            eval_duration_ms: 100,
            estimated: false,
        });
        assert!(!telem.context_estimated);
        assert!(!telem.speed_estimated);
        assert!(telem.totals_estimated);
    }

    #[test]
    fn latest_call_restores_from_history_and_clears_with_empty_history() {
        let usage = TurnTelemetry {
            context: Some(crate::ContextBreakdown::default()),
            prompt_tokens: 800,
            completion_tokens: 200,
            eval_duration_ms: 4000,
            estimated: true,
        };
        let mut telemetry = SessionTelemetry::default();
        telemetry.record_turn(&usage);
        assert_eq!(telemetry.last_call, Some(usage));
        telemetry.restore_from_conversation(&[msg_entry(1, Role::Assistant, "reply", Some(usage))]);
        assert_eq!(telemetry.last_call, Some(usage));
        let mut legacy = serde_json::to_value(&telemetry).unwrap();
        legacy.as_object_mut().unwrap().remove("last_call");
        assert!(
            serde_json::from_value::<SessionTelemetry>(legacy)
                .unwrap()
                .last_call
                .is_none()
        );
        telemetry.restore_from_conversation(&[]);
        assert!(telemetry.last_call.is_none());
        assert!(telemetry.context.is_none());
        assert!(telemetry.current_speed_tps.is_none());
    }

    fn msg_entry(
        id: i64,
        role: Role,
        content: &str,
        usage: Option<TurnTelemetry>,
    ) -> ConversationEntry {
        ConversationEntry::Message(ChatMessage {
            id,
            session_id: 1,
            role,
            content: content.into(),
            created_at: 0,
            tool_calls: None,
            tool_call_id: None,
            usage,
        })
    }

    #[test]
    fn test_restore_from_conversation_uses_last_usage_and_counts_tool_steps() {
        let entries = vec![
            msg_entry(
                1,
                Role::User,
                "hi",
                Some(TurnTelemetry {
                    context: None,
                    prompt_tokens: 10,
                    completion_tokens: 0,
                    eval_duration_ms: 0,
                    estimated: false,
                }),
            ),
            ConversationEntry::ToolStep(ToolStep {
                timing: None,
                anchor_message_id: 1,
                checkpoint: None,
                tool_call_id: "call_1".into(),
                name: "read_file".into(),
                summary: "read a.rs".into(),
                ok: Some(true),
                result_summary: None,
                diff: None,
            }),
            msg_entry(
                2,
                Role::Assistant,
                "done",
                Some(TurnTelemetry {
                    context: None,
                    prompt_tokens: 30,
                    completion_tokens: 12,
                    eval_duration_ms: 400,
                    estimated: false,
                }),
            ),
        ];

        let mut telem = SessionTelemetry::default();
        telem.restore_from_conversation(&entries);

        assert_eq!(telem.context_tokens, 42);
        assert!(!telem.context_estimated);
        assert_eq!(telem.total_prompt_tokens, 40);
        assert_eq!(telem.total_completion_tokens, 12);
        assert_eq!(telem.tool_calls_count, 1);
    }

    #[test]
    fn test_restore_from_conversation_estimates_when_no_usage() {
        let entries = vec![msg_entry(1, Role::User, "fix the bug in main.rs", None)];

        let mut telem = SessionTelemetry::default();
        telem.restore_from_conversation(&entries);

        assert!(telem.context_tokens > 0);
        assert!(telem.context_estimated);
    }

    #[test]
    fn test_extract_editor_context_prelude() {
        let combined = "<active_editor_context>\nActive file: `main.rs` (cursor at line 1, col 1)\n</active_editor_context>\n\nRefactor this function";
        let (ctx, prompt) = extract_editor_context_prelude(combined);
        assert!(ctx.is_some());
        assert!(ctx.unwrap().contains("Active file: `main.rs`"));
        assert_eq!(prompt, "Refactor this function");

        let plain = "Regular message without context";
        let (ctx, prompt) = extract_editor_context_prelude(plain);
        assert!(ctx.is_none());
        assert_eq!(prompt, plain);
    }

    #[test]
    fn test_estimate_tokens_accuracy() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello"), 2);
        assert!(estimate_tokens("fn main() {\n    println!(\"hello\");\n}") >= 8);
    }

    #[test]
    fn test_calculate_conversation_telemetry() {
        let entries = vec![
            ConversationEntry::Message(ChatMessage {
                id: 1,
                session_id: 1,
                role: Role::User,
                content: "Fix the bug in main.rs".into(),
                created_at: 0,
                tool_calls: None,
                tool_call_id: None,
                usage: None,
            }),
            ConversationEntry::ToolStep(ToolStep {
                timing: None,
                anchor_message_id: 1,
                checkpoint: None,
                tool_call_id: "call_1".into(),
                name: "read_file".into(),
                summary: "read src/main.rs".into(),
                ok: Some(true),
                result_summary: Some("read 20 lines".into()),
                diff: None,
            }),
            ConversationEntry::Message(ChatMessage {
                id: 2,
                session_id: 1,
                role: Role::Assistant,
                content: "I found and fixed the bug.".into(),
                created_at: 0,
                tool_calls: None,
                tool_call_id: None,
                usage: None,
            }),
        ];

        let (p_tok, c_tok, tools) = calculate_conversation_telemetry(&entries);
        assert!(p_tok > 10);
        assert!(c_tok > 5);
        assert_eq!(tools, 1);
    }
}
