//! Typed tool calls.
//!
//! A wire-level [`ToolCall`] is parsed into a [`Tool`] whose variants carry
//! typed argument structs, so malformed JSON arguments (a truncated streamed
//! call, a model typo) become a [`ToolArgError`] instead of silently running
//! a tool with empty defaults. [`ToolName`] is the single source of truth for
//! the tool set: its wire names, JSON schemas, and approval policy.

use std::fmt;
use std::str::FromStr;

use openwebide_core::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;

/// Arguments for `read_file`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
    /// First line (1-based; default 1).
    pub offset: Option<u64>,
    /// Line count (default 2000).
    pub limit: Option<u64>,
}

/// Arguments for `write_file`.
#[derive(Debug, Clone, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

/// Arguments for `edit_file`.
#[derive(Debug, Clone, Deserialize)]
pub struct EditFileArgs {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

/// Arguments for `list_dir`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListDirArgs {
    pub path: Option<String>,
}

/// Arguments for `search`.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    pub path: Option<String>,
    /// Include .git, target, node_modules, dist; default false.
    pub include_ignored: Option<bool>,
}

/// Arguments for `grep_search`.
#[derive(Debug, Clone, Deserialize)]
pub struct GrepSearchArgs {
    pub query: String,
    pub path: Option<String>,
    /// Include .git, target, node_modules, dist; default false.
    pub include_ignored: Option<bool>,
}

/// Arguments for `search_web`.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchWebArgs {
    pub query: String,
    pub limit: Option<u64>,
}

/// Arguments for `fetch_web_page`.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchWebPageArgs {
    pub url: String,
}

/// Arguments for `run_command`.
#[derive(Debug, Clone, Deserialize)]
pub struct RunCommandArgs {
    pub command: String,
    pub timeout_seconds: Option<u64>,
}

/// Arguments for `git_diff`.
#[derive(Debug, Clone, Deserialize)]
pub struct GitDiffArgs {
    pub path: Option<String>,
}

/// Arguments for `git_commit`.
#[derive(Debug, Clone, Deserialize)]
pub struct GitCommitArgs {
    pub message: String,
    pub paths: Option<Vec<String>>,
}

/// Arguments for `git_branch`.
#[derive(Debug, Clone, Deserialize)]
pub struct GitBranchArgs {
    pub branch_name: String,
    /// Forward-compatible field: deserialized now, wired up in a later step.
    pub create: Option<bool>,
}

/// A parsed tool call: the tool plus its typed arguments.
#[derive(Debug, Clone)]
pub enum Tool {
    ReadFile(ReadFileArgs),
    WriteFile(WriteFileArgs),
    EditFile(EditFileArgs),
    ListDir(ListDirArgs),
    Search(SearchArgs),
    GrepSearch(GrepSearchArgs),
    SearchWeb(SearchWebArgs),
    FetchWebPage(FetchWebPageArgs),
    RunCommand(RunCommandArgs),
    GitStatus,
    HostInfo,
    TodoWrite(openwebide_core::TodoPlan),
    AskUserQuestion(openwebide_core::questions::QuestionRequest),
    Memory(openwebide_core::MemoryCommand),
    Skill(openwebide_core::SkillCommand),
    SkillCreator(crate::skills::CreatorArgs),
    SkillList(crate::skills::ListArgs),
    SkillRead(crate::skills::ReadArgs),
    Scheduled(openwebide_core::scheduled::TaskCommand),
    GitDiff(GitDiffArgs),
    GitCommit(GitCommitArgs),
    GitBranch(GitBranchArgs),
}

impl Tool {
    /// A short human-readable description of what this call will do, shown
    /// in the UI before the tool runs.
    pub fn describe(&self) -> String {
        match self {
            Tool::ReadFile(args) => format!("read {}", args.path),
            Tool::WriteFile(args) => {
                let n = args.content.lines().count();
                let bytes = args.content.len();
                format!("write {} ({n} lines, {bytes} B)", args.path)
            }
            Tool::EditFile(args) => {
                let old_lines = args.old_string.lines().count().max(1);
                let new_lines = args.new_string.lines().count();
                format!("edit {} (-{old_lines}/+{new_lines} lines)", args.path)
            }
            Tool::ListDir(args) => format!("list {}", args.path.as_deref().unwrap_or_default()),
            Tool::Search(args) => format!("search '{}'", args.query),
            Tool::GrepSearch(args) => format!("grep '{}'", args.query),
            Tool::SearchWeb(args) => format!("search web for '{}'", args.query),
            Tool::FetchWebPage(args) => format!("fetch web page {}", args.url),
            Tool::RunCommand(args) => format!("run '{}'", args.command),
            Tool::GitStatus => "inspect git status".to_string(),
            Tool::HostInfo => "inspect bridge host hardware".into(),
            Tool::Scheduled(command) => match command {
                openwebide_core::scheduled::TaskCommand::Monitor { command, .. } => match command {
                    openwebide_core::scheduled::MonitorCommand::Start { delay_seconds, .. } => {
                        format!("Start a host monitor in {delay_seconds} seconds")
                    }
                    openwebide_core::scheduled::MonitorCommand::List {} => {
                        "List conversation monitors".into()
                    }
                    openwebide_core::scheduled::MonitorCommand::Cancel { id, .. } => {
                        format!("Cancel future checks for monitor #{id}")
                    }
                },
                openwebide_core::scheduled::TaskCommand::List => "list scheduled tasks".into(),
                openwebide_core::scheduled::TaskCommand::Create { draft } => {
                    format!("schedule {}", draft.title)
                }
                openwebide_core::scheduled::TaskCommand::Update { id, draft, .. } => {
                    format!("update scheduled task {id}: {}", draft.title)
                }
                openwebide_core::scheduled::TaskCommand::Delete { id, .. } => {
                    format!("delete scheduled task {id}")
                }
                openwebide_core::scheduled::TaskCommand::SetEnabled { .. } => {
                    "toggle scheduled task".into()
                }
            },
            Tool::SkillList(args) => format!("list skills matching '{}'", args.query),
            Tool::SkillRead(args) => format!("read skill {}", args.id),
            Tool::SkillCreator(_) => "create or improve a project skill".into(),
            Tool::Skill(command) => match command {
                openwebide_core::SkillCommand::List { query } => {
                    format!("list skills matching '{query}'")
                }
                openwebide_core::SkillCommand::Read { id, resource } => format!(
                    "read skill {id}{}",
                    resource
                        .as_ref()
                        .map_or_else(String::new, |name| format!(": {name}"))
                ),
                openwebide_core::SkillCommand::Create { draft } => {
                    format!("create skill {}", draft.name)
                }
                openwebide_core::SkillCommand::Update { id, draft, .. } => {
                    format!("update skill {id}: {}", draft.name)
                }
                openwebide_core::SkillCommand::Delete { id, .. } => format!("delete skill {id}"),
                openwebide_core::SkillCommand::SetEnabled { .. } => "toggle project skills".into(),
            },
            Tool::Memory(command) => match command {
                openwebide_core::MemoryCommand::Create { title, .. } => format!("remember {title}"),
                openwebide_core::MemoryCommand::Search { query } => {
                    format!("search project memories for '{query}'")
                }
                openwebide_core::MemoryCommand::Read { id } => format!("read project memory {id}"),
                openwebide_core::MemoryCommand::Update { id, title, .. } => {
                    format!("update project memory {id}: {title}")
                }
                openwebide_core::MemoryCommand::Delete { id, .. } => {
                    format!("delete project memory {id}")
                }
                openwebide_core::MemoryCommand::SetEnabled { .. } => "toggle project memory".into(),
            },
            Tool::AskUserQuestion(_) => "Ask the user for clarification".into(),
            Tool::TodoWrite(plan) => format!("update plan ({} items)", plan.todos.len()),
            Tool::GitDiff(args) => match args.path.as_deref() {
                Some(p) => format!("inspect git diff for '{p}'"),
                None => "inspect repository git diff".to_string(),
            },
            Tool::GitCommit(args) => {
                let paths = args.paths.as_deref().filter(|p| !p.is_empty());
                match paths {
                    Some(p) => format!("commit {}: '{}'", p.join(", "), args.message),
                    None => format!("commit ALL tracked changes: '{}'", args.message),
                }
            }
            Tool::GitBranch(args) => format!("switch to branch '{}'", args.branch_name),
        }
    }
}

/// The name of a built-in tool, matching the wire names sent to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolName {
    ReadFile,
    WriteFile,
    EditFile,
    ListDir,
    Search,
    GrepSearch,
    SearchWeb,
    FetchWebPage,
    RunCommand,
    GitStatus,
    HostInfo,
    TodoWrite,
    AskUserQuestion,
    Monitor,
    ScheduleList,
    ScheduleCreate,
    ScheduleUpdate,
    ScheduleDelete,
    MemoryCreate,
    MemorySearch,
    MemoryRead,
    MemoryUpdate,
    MemoryDelete,
    SkillList,
    SkillRead,
    SkillCreate,
    SkillUpdate,
    SkillDelete,
    SkillCreator,

    GitDiff,
    GitCommit,
    GitBranch,
}

impl ToolName {
    /// Whether execution can safely be interrupted before it finishes.
    pub fn cancellable(self) -> bool {
        matches!(
            self,
            Self::AskUserQuestion
                | Self::RunCommand
                | Self::FetchWebPage
                | Self::SearchWeb
                | Self::Search
                | Self::GrepSearch
        )
    }

    /// Every built-in tool. Memory tools are advertised only for enabled projects.
    pub const ALL: &[ToolName] = &[
        ToolName::ReadFile,
        ToolName::WriteFile,
        ToolName::EditFile,
        ToolName::ListDir,
        ToolName::Search,
        ToolName::GrepSearch,
        ToolName::SearchWeb,
        ToolName::FetchWebPage,
        ToolName::RunCommand,
        ToolName::GitStatus,
        ToolName::HostInfo,
        ToolName::TodoWrite,
        ToolName::AskUserQuestion,
        ToolName::Monitor,
        ToolName::ScheduleList,
        ToolName::ScheduleCreate,
        ToolName::ScheduleUpdate,
        ToolName::ScheduleDelete,
        ToolName::MemoryCreate,
        ToolName::MemorySearch,
        ToolName::MemoryRead,
        ToolName::MemoryUpdate,
        ToolName::MemoryDelete,
        ToolName::SkillList,
        ToolName::SkillRead,
        ToolName::SkillCreate,
        ToolName::SkillUpdate,
        ToolName::SkillDelete,
        ToolName::SkillCreator,
        ToolName::GitDiff,
        ToolName::GitCommit,
        ToolName::GitBranch,
    ];

    /// The wire name of the tool (e.g. `"read_file"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ToolName::ReadFile => "read_file",
            ToolName::WriteFile => "write_file",
            ToolName::EditFile => "edit_file",
            ToolName::ListDir => "list_dir",
            ToolName::Search => "search",
            ToolName::GrepSearch => "grep_search",
            ToolName::SearchWeb => "search_web",
            ToolName::FetchWebPage => "fetch_web_page",
            ToolName::RunCommand => "run_command",
            ToolName::GitStatus => "git_status",
            ToolName::HostInfo => "host_info",
            ToolName::AskUserQuestion => "ask_user_question",
            ToolName::TodoWrite => "todo_write",
            ToolName::Monitor => "monitor",
            ToolName::ScheduleList => "schedule_list",
            ToolName::ScheduleCreate => "schedule_create",
            ToolName::ScheduleUpdate => "schedule_update",
            ToolName::ScheduleDelete => "schedule_delete",
            ToolName::MemoryCreate => "memory_create",
            ToolName::MemorySearch => "memory_search",
            ToolName::MemoryRead => "memory_read",
            ToolName::MemoryUpdate => "memory_update",
            ToolName::MemoryDelete => "memory_delete",
            ToolName::SkillList => "skill_list",
            ToolName::SkillRead => "skill_read",
            ToolName::SkillCreate => "skill_create",
            ToolName::SkillUpdate => "skill_update",
            ToolName::SkillDelete => "skill_delete",
            ToolName::SkillCreator => "skill_creator",

            ToolName::GitDiff => "git_diff",
            ToolName::GitCommit => "git_commit",
            ToolName::GitBranch => "git_branch",
        }
    }

    /// The tool's JSON-schema definition, as advertised to the model.
    pub fn definition(self) -> ToolDefinition {
        match self {
            ToolName::SkillList | ToolName::SkillRead | ToolName::SkillCreate | ToolName::SkillUpdate | ToolName::SkillDelete | ToolName::SkillCreator => crate::skills::definition(self.as_str()),
            ToolName::Monitor | ToolName::ScheduleList | ToolName::ScheduleCreate | ToolName::ScheduleUpdate | ToolName::ScheduleDelete => crate::scheduled::definition(self.as_str()),
            ToolName::MemoryCreate => ToolDefinition { name: self.as_str().into(), description: "Store a durable project fact or convention. Omit title for an automatic name. Avoid secrets and transient state.".into(), parameters: json!({"type":"object","properties":{"auto_title":{"type":"boolean","description":"Generate title from content"},"title":{"type":"string","maxLength":120},"content":{"type":"string","maxLength":4000}},"required":["content"],"additionalProperties":false}) },
            ToolName::MemorySearch => ToolDefinition { name: self.as_str().into(), description: "Search project memories; empty query lists recent entries.".into(), parameters: json!({"type":"object","properties":{"query":{"type":"string","maxLength":256}},"required":["query"],"additionalProperties":false}) },
            ToolName::MemoryRead => ToolDefinition { name: self.as_str().into(), description: "Read a full project memory and its revision.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"integer","minimum":1}},"required":["id"],"additionalProperties":false}) },
            ToolName::MemoryUpdate => ToolDefinition { name: self.as_str().into(), description: "Update a project memory at its current revision; reread after conflicts.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"integer","minimum":1},"revision":{"type":"integer","minimum":1},"auto_title":{"type":"boolean"},"title":{"type":"string","maxLength":120},"content":{"type":"string","maxLength":4000}},"required":["id","revision","content"],"additionalProperties":false}) },
            ToolName::MemoryDelete => ToolDefinition { name: self.as_str().into(), description: "Delete a project memory at its current revision.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"integer","minimum":1},"revision":{"type":"integer","minimum":1}},"required":["id","revision"],"additionalProperties":false}) },
            ToolName::AskUserQuestion => crate::questions::definition(),
            ToolName::TodoWrite => ToolDefinition {
                name: "todo_write".into(),
                description: "Replace checklist: stable IDs, at most one in_progress; [] clears.".into(),
                parameters: json!({ "type": "object", "properties": { "todos": { "type": "array", "maxItems": 64, "items": { "type": "object", "properties": {
                    "id": { "type": "string", "maxLength": 64, "description": "Stable ID" },
                    "content": { "type": "string", "maxLength": 2048, "description": "Task" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                }, "required": ["id", "content", "status"] } } }, "required": ["todos"] }),
            },
            ToolName::ReadFile => ToolDefinition {
                name: "read_file".into(),
                description: "Read a workspace file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path" },
                        "offset": { "type": "integer", "description": "1-based start line" },
                        "limit": { "type": "integer", "description": "Line count" }
                    },
                    "required": ["path"]
                }),
            },
            ToolName::WriteFile => ToolDefinition {
                name: "write_file".into(),
                description: "Create or overwrite a full workspace file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolName::EditFile => ToolDefinition {
                name: "edit_file".into(),
                description: "Replace one unique exact block in an existing file. Prefer over write_file for edits.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path" },
                        "old_string": { "type": "string", "description": "Unique existing text" },
                        "new_string": { "type": "string", "description": "Replacement text" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
            ToolName::ListDir => ToolDefinition {
                name: "list_dir".into(),
                description: "List a workspace directory.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory; empty for root" }
                    }
                }),
            },
            ToolName::Search => ToolDefinition {
                name: "search".into(),
                description: "Find workspace file names matching a substring.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "path": { "type": "string", "description": "Directory; empty for root" },
                        "include_ignored": { "type": "boolean", "description": "Include ignored dirs; default false" }
                    },
                    "required": ["query"]
                }),
            },
            ToolName::GrepSearch => ToolDefinition {
                name: "grep_search".into(),
                description: "Find workspace lines matching a substring.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "path": { "type": "string", "description": "Directory; empty for root" },
                        "include_ignored": { "type": "boolean", "description": "Include ignored dirs; default false" }
                    },
                    "required": ["query"]
                }),
            },
            ToolName::SearchWeb => ToolDefinition {
                name: "search_web".into(),
                description: "Search the web.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "description": "Default 5, max 10" }
                    },
                    "required": ["query"]
                }),
            },
            ToolName::FetchWebPage => ToolDefinition {
                name: "fetch_web_page".into(),
                description: "Fetch an HTTP(S) page as Markdown.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" }
                    },
                    "required": ["url"]
                }),
            },
            ToolName::RunCommand => ToolDefinition {
                name: "run_command".into(),
                description: "Run a shell command in the project directory.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command"
                        },
                        "timeout_seconds": {
                            "type": "integer",
                            "description": "Default 30s, max 300s"
                        }
                    },
                    "required": ["command"]
                }),
            },
            ToolName::HostInfo => ToolDefinition {
                name: "host_info".into(),
                description: "Read bridge-host CPU, RAM, disks, GPUs and sensors.".into(),
                parameters: json!({"type":"object", "properties":{}, "additionalProperties":false}),
            },
            ToolName::GitStatus => ToolDefinition {
                name: "git_status".into(),
                description: "Show branch, uncommitted and untracked Git status.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolName::GitDiff => ToolDefinition {
                name: "git_diff".into(),
                description: "Show uncommitted Git diff.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Optional file path" }
                    }
                }),
            },
            ToolName::GitCommit => ToolDefinition {
                name: "git_commit".into(),
                description: "Commit selected paths (omit paths for all tracked changes).".into(),
                parameters: json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string", "description": "Conventional commit message" },
                        "paths": { "type": "array", "items": { "type": "string" }, "description": "Files; omit for all tracked" }
                    }
                }),
            },
            ToolName::GitBranch => ToolDefinition {
                name: "git_branch".into(),
                description: "Create and switch to a Git branch.".into(),
                parameters: json!({
                    "type": "object",
                    "required": ["branch_name"],
                    "properties": {
                        "branch_name": { "type": "string", "description": "New branch name" }
                    }
                }),
            },
        }
    }

    /// Whether the tool needs explicit user approval before it runs.
    ///
    /// Read-only tools run automatically; everything else (writes, shell
    /// execution, git mutations, external fetches) is gated.
    pub fn requires_approval(self) -> bool {
        !matches!(
            self,
            ToolName::ReadFile
                | ToolName::ListDir
                | ToolName::Search
                | ToolName::GrepSearch
                | ToolName::HostInfo
                | ToolName::AskUserQuestion
                | ToolName::TodoWrite
                | ToolName::ScheduleList
                | ToolName::SkillList
                | ToolName::SkillRead
                | ToolName::SkillCreator
                | ToolName::MemoryRead
                | ToolName::MemorySearch
                | ToolName::GitStatus
                | ToolName::GitDiff
                | ToolName::SearchWeb
        )
    }

    /// Whether the tool may be auto-approved for the whole session.
    pub fn always_approvable(self) -> bool {
        !matches!(self, ToolName::RunCommand)
    }

    /// Whether the tool runs through the bridge daemon rather than the VFS.
    pub fn needs_bridge(self) -> bool {
        matches!(
            self,
            ToolName::RunCommand
                | ToolName::HostInfo
                | ToolName::GitStatus
                | ToolName::GitDiff
                | ToolName::GitCommit
                | ToolName::GitBranch
        )
    }
}

/// Returned by [`ToolName::from_str`] for a name that is not a built-in tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownToolName;

impl FromStr for ToolName {
    type Err = UnknownToolName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read_file" => Ok(ToolName::ReadFile),
            "write_file" => Ok(ToolName::WriteFile),
            "edit_file" => Ok(ToolName::EditFile),
            "list_dir" => Ok(ToolName::ListDir),
            "search" => Ok(ToolName::Search),
            "grep_search" => Ok(ToolName::GrepSearch),
            "search_web" => Ok(ToolName::SearchWeb),
            "fetch_web_page" => Ok(ToolName::FetchWebPage),
            "run_command" => Ok(ToolName::RunCommand),
            "git_status" => Ok(ToolName::GitStatus),
            "host_info" => Ok(ToolName::HostInfo),
            "ask_user_question" => Ok(ToolName::AskUserQuestion),
            "todo_write" => Ok(ToolName::TodoWrite),
            "monitor" => Ok(ToolName::Monitor),
            "schedule_list" => Ok(ToolName::ScheduleList),
            "schedule_create" => Ok(ToolName::ScheduleCreate),
            "schedule_update" => Ok(ToolName::ScheduleUpdate),
            "schedule_delete" => Ok(ToolName::ScheduleDelete),
            "memory_create" => Ok(ToolName::MemoryCreate),
            "memory_search" => Ok(ToolName::MemorySearch),
            "memory_read" => Ok(ToolName::MemoryRead),
            "memory_update" => Ok(ToolName::MemoryUpdate),
            "memory_delete" => Ok(ToolName::MemoryDelete),
            "skill_list" => Ok(ToolName::SkillList),
            "skill_read" => Ok(ToolName::SkillRead),
            "skill_create" => Ok(ToolName::SkillCreate),
            "skill_update" => Ok(ToolName::SkillUpdate),
            "skill_delete" => Ok(ToolName::SkillDelete),
            "skill_creator" => Ok(ToolName::SkillCreator),

            "git_diff" => Ok(ToolName::GitDiff),
            "git_commit" => Ok(ToolName::GitCommit),
            "git_branch" => Ok(ToolName::GitBranch),
            _ => Err(UnknownToolName),
        }
    }
}

/// Why a tool call could not be parsed into a [`Tool`].
#[derive(Debug, Clone)]
pub enum ToolArgError {
    /// The call names a tool the executor does not implement.
    UnknownTool(String),
    /// The call's arguments are not valid JSON for the named tool.
    InvalidArguments { tool: &'static str, error: String },
}

impl fmt::Display for ToolArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolArgError::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            ToolArgError::InvalidArguments { tool, error } => {
                write!(f, "invalid arguments for {tool}: {error}")
            }
        }
    }
}

/// Parse a wire-level tool call into a typed [`Tool`].
///
/// An unknown tool name is [`ToolArgError::UnknownTool`]. `git_status` takes
/// no arguments, so its `arguments` are ignored (even when malformed). An
/// empty `arguments` string is treated as `{}`; malformed JSON or wrong field
/// types are [`ToolArgError::InvalidArguments`].
pub fn parse(call: &ToolCall) -> Result<Tool, ToolArgError> {
    let name: ToolName = call
        .name
        .parse()
        .map_err(|_| ToolArgError::UnknownTool(call.name.clone()))?;
    if name == ToolName::GitStatus {
        // The git_status handler never looks at the arguments, so don't
        // fail a call just because its payload is empty or malformed.
        return Ok(Tool::GitStatus);
    }
    let raw = call.arguments.trim();
    let raw = if raw.is_empty() { "{}" } else { raw };
    macro_rules! parse_as {
        ($variant:ident, $ty:ty) => {
            serde_json::from_str::<$ty>(raw)
                .map(Tool::$variant)
                .map_err(|e| ToolArgError::InvalidArguments {
                    tool: name.as_str(),
                    error: e.to_string(),
                })
        };
    }
    match name {
        ToolName::Monitor => {
            serde_json::from_str::<openwebide_core::scheduled::MonitorCommand>(raw)
                .map(|command| {
                    Tool::Scheduled(openwebide_core::scheduled::TaskCommand::Monitor {
                        session_id: 0,
                        command,
                    })
                })
                .map_err(|error| ToolArgError::InvalidArguments {
                    tool: "monitor",
                    error: error.to_string(),
                })
        }
        ToolName::ScheduleList
        | ToolName::ScheduleCreate
        | ToolName::ScheduleUpdate
        | ToolName::ScheduleDelete => {
            let parsed = (|| {
                let mut value: serde_json::Value = serde_json::from_str(raw)?;
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| serde::de::Error::custom("Expected an object"))?;
                if object.contains_key("action") {
                    return Err(serde::de::Error::custom("Unexpected action"));
                }
                object.insert(
                    "action".into(),
                    serde_json::Value::String(name.as_str().trim_start_matches("schedule_").into()),
                );
                if let Some(draft) = object
                    .get_mut("draft")
                    .and_then(serde_json::Value::as_object_mut)
                    && !draft.contains_key("auto_title")
                    && draft
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|title| title.trim().is_empty())
                {
                    draft.insert("auto_title".into(), serde_json::Value::Bool(true));
                }
                serde_json::from_value::<openwebide_core::scheduled::TaskCommand>(value)
            })();
            parsed
                .map(Tool::Scheduled)
                .map_err(|error: serde_json::Error| ToolArgError::InvalidArguments {
                    tool: name.as_str(),
                    error: error.to_string(),
                })
        }
        ToolName::MemoryCreate
        | ToolName::MemorySearch
        | ToolName::MemoryRead
        | ToolName::MemoryUpdate
        | ToolName::MemoryDelete => {
            let parsed = (|| {
                let mut value: serde_json::Value = serde_json::from_str(raw)?;
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| serde::de::Error::custom("Expected an object"))?;
                if object.contains_key("action") {
                    return Err(serde::de::Error::custom("Unexpected action field"));
                }
                object.insert(
                    "action".into(),
                    serde_json::Value::String(name.as_str().trim_start_matches("memory_").into()),
                );
                if matches!(name, ToolName::MemoryCreate | ToolName::MemoryUpdate)
                    && !object.contains_key("auto_title")
                    && object
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|title| title.trim().is_empty())
                {
                    object.insert("auto_title".into(), serde_json::Value::Bool(true));
                }
                serde_json::from_value::<openwebide_core::MemoryCommand>(value)
            })();
            parsed
                .map(Tool::Memory)
                .map_err(|error: serde_json::Error| ToolArgError::InvalidArguments {
                    tool: name.as_str(),
                    error: error.to_string(),
                })
        }
        ToolName::SkillList => parse_as!(SkillList, crate::skills::ListArgs),
        ToolName::SkillRead => parse_as!(SkillRead, crate::skills::ReadArgs),
        ToolName::SkillCreator => parse_as!(SkillCreator, crate::skills::CreatorArgs),
        ToolName::SkillCreate | ToolName::SkillUpdate | ToolName::SkillDelete => {
            let parsed = (|| {
                let mut value: serde_json::Value = serde_json::from_str(raw)?;
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| serde::de::Error::custom("Expected an object"))?;
                if object.contains_key("action") {
                    return Err(serde::de::Error::custom("Unexpected action field"));
                }
                object.insert(
                    "action".into(),
                    serde_json::Value::String(name.as_str().trim_start_matches("skill_").into()),
                );
                serde_json::from_value::<openwebide_core::SkillCommand>(value)
            })();
            parsed.map(Tool::Skill).map_err(|error: serde_json::Error| {
                ToolArgError::InvalidArguments {
                    tool: name.as_str(),
                    error: error.to_string(),
                }
            })
        }
        ToolName::AskUserQuestion => {
            parse_as!(AskUserQuestion, openwebide_core::questions::QuestionRequest)
        }
        ToolName::TodoWrite => parse_as!(TodoWrite, openwebide_core::TodoPlan),
        ToolName::ReadFile => parse_as!(ReadFile, ReadFileArgs),
        ToolName::WriteFile => parse_as!(WriteFile, WriteFileArgs),
        ToolName::EditFile => parse_as!(EditFile, EditFileArgs),
        ToolName::ListDir => parse_as!(ListDir, ListDirArgs),
        ToolName::Search => parse_as!(Search, SearchArgs),
        ToolName::GrepSearch => parse_as!(GrepSearch, GrepSearchArgs),
        ToolName::SearchWeb => parse_as!(SearchWeb, SearchWebArgs),
        ToolName::FetchWebPage => parse_as!(FetchWebPage, FetchWebPageArgs),
        ToolName::RunCommand => parse_as!(RunCommand, RunCommandArgs),
        ToolName::HostInfo => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct NoArgs {}
            serde_json::from_str::<NoArgs>(raw)
                .map(|_| Tool::HostInfo)
                .map_err(|error| ToolArgError::InvalidArguments {
                    tool: "host_info",
                    error: error.to_string(),
                })
        }
        ToolName::GitStatus => unreachable!("handled above"),
        ToolName::GitDiff => parse_as!(GitDiff, GitDiffArgs),
        ToolName::GitCommit => parse_as!(GitCommit, GitCommitArgs),
        ToolName::GitBranch => parse_as!(GitBranch, GitBranchArgs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn parse_each_tool_with_valid_args() {
        let cases = [
            (
                "read_file",
                r#"{"path": "src/main.rs"}"#,
                ToolName::ReadFile,
            ),
            (
                "write_file",
                r#"{"path": "src/main.rs", "content": "fn main() {}"}"#,
                ToolName::WriteFile,
            ),
            (
                "edit_file",
                r#"{"path": "src/main.rs", "old_string": "fn main() {}", "new_string": "fn main() { println!(\"hi\"); }"}"#,
                ToolName::EditFile,
            ),
            ("list_dir", r#"{"path": "src"}"#, ToolName::ListDir),
            ("list_dir", r#"{}"#, ToolName::ListDir),
            ("search", r#"{"query": "main"}"#, ToolName::Search),
            (
                "grep_search",
                r#"{"query": "fn main", "path": "src"}"#,
                ToolName::GrepSearch,
            ),
            (
                "search_web",
                r#"{"query": "rust wasm", "limit": 3}"#,
                ToolName::SearchWeb,
            ),
            (
                "fetch_web_page",
                r#"{"url": "https://doc.rust-lang.org"}"#,
                ToolName::FetchWebPage,
            ),
            (
                "run_command",
                r#"{"command": "cargo test", "timeout_seconds": 60}"#,
                ToolName::RunCommand,
            ),
            ("git_status", r#"{}"#, ToolName::GitStatus),
            ("git_diff", r#"{"path": "src/main.rs"}"#, ToolName::GitDiff),
            (
                "git_commit",
                r#"{"message": "feat: x", "paths": ["a.rs", "b.rs"]}"#,
                ToolName::GitCommit,
            ),
            (
                "git_branch",
                r#"{"branch_name": "feat/x"}"#,
                ToolName::GitBranch,
            ),
        ];
        for (name, arguments, expected) in cases {
            let tool = parse(&call(name, arguments))
                .unwrap_or_else(|e| panic!("parse({name}, {arguments}) failed: {e}"));
            assert_eq!(
                tool_name_of(&tool),
                expected,
                "parse({name}) matched the wrong variant"
            );
        }
    }

    fn tool_name_of(tool: &Tool) -> ToolName {
        match tool {
            Tool::ReadFile(_) => ToolName::ReadFile,
            Tool::WriteFile(_) => ToolName::WriteFile,
            Tool::EditFile(_) => ToolName::EditFile,
            Tool::ListDir(_) => ToolName::ListDir,
            Tool::Search(_) => ToolName::Search,
            Tool::GrepSearch(_) => ToolName::GrepSearch,
            Tool::SearchWeb(_) => ToolName::SearchWeb,
            Tool::FetchWebPage(_) => ToolName::FetchWebPage,
            Tool::RunCommand(_) => ToolName::RunCommand,
            Tool::GitStatus => ToolName::GitStatus,
            Tool::HostInfo => ToolName::HostInfo,
            Tool::Scheduled(command) => match command {
                openwebide_core::scheduled::TaskCommand::Monitor { .. } => ToolName::Monitor,
                openwebide_core::scheduled::TaskCommand::List => ToolName::ScheduleList,
                openwebide_core::scheduled::TaskCommand::Create { .. } => ToolName::ScheduleCreate,
                openwebide_core::scheduled::TaskCommand::Update { .. } => ToolName::ScheduleUpdate,
                openwebide_core::scheduled::TaskCommand::Delete { .. } => ToolName::ScheduleDelete,
                openwebide_core::scheduled::TaskCommand::SetEnabled { .. } => unreachable!(),
            },
            Tool::SkillList(_) => ToolName::SkillList,
            Tool::SkillRead(_) => ToolName::SkillRead,
            Tool::SkillCreator(_) => ToolName::SkillCreator,
            Tool::Skill(command) => match command {
                openwebide_core::SkillCommand::List { .. } => ToolName::SkillList,
                openwebide_core::SkillCommand::Read { .. } => ToolName::SkillRead,
                openwebide_core::SkillCommand::Create { .. } => ToolName::SkillCreate,
                openwebide_core::SkillCommand::Update { .. } => ToolName::SkillUpdate,
                openwebide_core::SkillCommand::Delete { .. } => ToolName::SkillDelete,
                openwebide_core::SkillCommand::SetEnabled { .. } => unreachable!(),
            },
            Tool::Memory(command) => match command {
                openwebide_core::MemoryCommand::Create { .. } => ToolName::MemoryCreate,
                openwebide_core::MemoryCommand::Search { .. } => ToolName::MemorySearch,
                openwebide_core::MemoryCommand::Read { .. } => ToolName::MemoryRead,
                openwebide_core::MemoryCommand::Update { .. } => ToolName::MemoryUpdate,
                openwebide_core::MemoryCommand::Delete { .. } => ToolName::MemoryDelete,
                openwebide_core::MemoryCommand::SetEnabled { .. } => unreachable!(),
            },
            Tool::AskUserQuestion(_) => ToolName::AskUserQuestion,
            Tool::TodoWrite(_) => ToolName::TodoWrite,
            Tool::GitDiff(_) => ToolName::GitDiff,
            Tool::GitCommit(_) => ToolName::GitCommit,
            Tool::GitBranch(_) => ToolName::GitBranch,
        }
    }

    #[test]
    fn parse_write_file_wrong_type_reports_the_mismatch() {
        let err = parse(&call("write_file", r#"{"path": 3}"#)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.starts_with("invalid arguments for write_file"),
            "unexpected message: {msg}"
        );
        // serde_json's Display reports the type mismatch itself; it does not
        // include the field name for value-level errors.
        assert!(
            msg.contains("invalid type: integer `3`, expected a string"),
            "unexpected message: {msg}"
        );
        match err {
            ToolArgError::InvalidArguments { tool, .. } => assert_eq!(tool, "write_file"),
            other @ ToolArgError::UnknownTool(_) => {
                panic!("expected InvalidArguments, got {other:?}")
            }
        }
    }

    #[test]
    fn parse_truncated_json_is_invalid_arguments() {
        let err = parse(&call("write_file", r#"{"path":"a","con"#)).unwrap_err();
        match err {
            ToolArgError::InvalidArguments { tool, .. } => assert_eq!(tool, "write_file"),
            other @ ToolArgError::UnknownTool(_) => {
                panic!("expected InvalidArguments, got {other:?}")
            }
        }
    }

    #[test]
    fn parse_git_status_ignores_arguments() {
        assert!(matches!(
            parse(&call("git_status", "")).unwrap(),
            Tool::GitStatus
        ));
        // Even malformed arguments don't matter: git_status takes none.
        assert!(matches!(
            parse(&call("git_status", r#"{"path":"a","con"#)).unwrap(),
            Tool::GitStatus
        ));
    }

    #[test]
    fn parse_unknown_tool_name() {
        let err = parse(&call("rm_rf", r#"{}"#)).unwrap_err();
        assert_eq!(err.to_string(), "unknown tool: rm_rf");
        match err {
            ToolArgError::UnknownTool(name) => assert_eq!(name, "rm_rf"),
            other @ ToolArgError::InvalidArguments { .. } => {
                panic!("expected UnknownTool, got {other:?}")
            }
        }
    }

    #[test]
    fn parse_ignores_extra_fields() {
        let tool = parse(&call(
            "read_file",
            r#"{"path": "src/main.rs", "stray_field": 42}"#,
        ))
        .unwrap();
        match tool {
            Tool::ReadFile(args) => assert_eq!(args.path, "src/main.rs"),
            other => panic!("expected ReadFile, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_arguments_become_empty_object() {
        // A tool with optional-only fields parses fine from an empty payload.
        let tool = parse(&call("list_dir", "  ")).unwrap();
        match tool {
            Tool::ListDir(args) => assert!(args.path.is_none()),
            other => panic!("expected ListDir, got {other:?}"),
        }
    }

    #[test]
    fn tool_name_round_trips_through_wire_names() {
        for name in ToolName::ALL {
            assert_eq!(name.as_str().parse::<ToolName>().unwrap(), *name);
        }
        assert!(matches!(
            "not_a_tool".parse::<ToolName>(),
            Err(UnknownToolName)
        ));
    }

    #[test]
    fn tool_name_policy_methods_match_the_lists() {
        let auto: &[ToolName] = &[
            ToolName::ScheduleList,
            ToolName::ReadFile,
            ToolName::ListDir,
            ToolName::Search,
            ToolName::GrepSearch,
            ToolName::GitStatus,
            ToolName::HostInfo,
            ToolName::TodoWrite,
            ToolName::SkillList,
            ToolName::SkillRead,
            ToolName::SkillCreator,
            ToolName::AskUserQuestion,
            ToolName::MemorySearch,
            ToolName::MemoryRead,
            ToolName::GitDiff,
            ToolName::SearchWeb,
        ];
        for name in ToolName::ALL {
            assert_eq!(
                !name.requires_approval(),
                auto.contains(name),
                "requires_approval drifted for {name:?}"
            );
            assert_eq!(
                name.always_approvable(),
                !matches!(name, ToolName::RunCommand),
                "always_approvable drifted for {name:?}"
            );
            assert_eq!(
                name.needs_bridge(),
                matches!(
                    name,
                    ToolName::RunCommand
                        | ToolName::HostInfo
                        | ToolName::GitStatus
                        | ToolName::GitDiff
                        | ToolName::GitCommit
                        | ToolName::GitBranch
                ),
                "needs_bridge drifted for {name:?}"
            );
        }
    }

    #[test]
    fn monitor_arguments_are_typed_and_cannot_redirect_the_session() {
        for arguments in [
            r#"{"action":"start","prompt":"Check the build","delay_seconds":600}"#,
            r#"{"action":"list"}"#,
            r#"{"action":"cancel","id":1,"revision":1}"#,
        ] {
            assert!(matches!(
                parse(&call("monitor", arguments)).unwrap(),
                Tool::Scheduled(openwebide_core::scheduled::TaskCommand::Monitor {
                    session_id: 0,
                    ..
                })
            ));
        }
        for arguments in [
            r#"{"action":"list","session_id":2}"#,
            r#"{"action":"start","prompt":"check"}"#,
            r#"{"action":"cancel","id":1}"#,
            r#"{"action":"unknown"}"#,
        ] {
            assert!(parse(&call("monitor", arguments)).is_err());
        }
    }

    #[test]
    fn describe_uses_typed_fields() {
        assert_eq!(
            parse(&call("read_file", r#"{"path": "src/main.rs"}"#))
                .unwrap()
                .describe(),
            "read src/main.rs"
        );
        assert_eq!(
            parse(&call(
                "write_file",
                r#"{"path": "src/main.rs", "content": "a\nb\n"}"#
            ))
            .unwrap()
            .describe(),
            "write src/main.rs (2 lines, 4 B)"
        );
        assert_eq!(
            parse(&call("list_dir", r#"{}"#)).unwrap().describe(),
            "list "
        );
        assert_eq!(
            parse(&call("git_commit", r#"{"message": "feat: x"}"#))
                .unwrap()
                .describe(),
            "commit ALL tracked changes: 'feat: x'"
        );
        assert_eq!(
            parse(&call(
                "git_commit",
                r#"{"message": "feat: x", "paths": ["a.rs", "b.rs"]}"#
            ))
            .unwrap()
            .describe(),
            "commit a.rs, b.rs: 'feat: x'"
        );
        // An empty paths array still reads as "commit ALL".
        assert_eq!(
            parse(&call(
                "git_commit",
                r#"{"message": "feat: x", "paths": []}"#
            ))
            .unwrap()
            .describe(),
            "commit ALL tracked changes: 'feat: x'"
        );
    }
}
