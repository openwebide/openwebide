//! Shared skill tools and authoring workflow above thin persistence adapters.
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use openwebide_core::{
    FileDiff, ProjectSkills, SkillCommand, ToolCall, ToolDefinition, rewind::ProjectSnapshot,
};
use serde::Deserialize;
use std::future::Future;

pub const TOOL_NAMES: &[&str] = &[
    "skill_list",
    "skill_read",
    "skill_create",
    "skill_update",
    "skill_delete",
    "skill_creator",
];
pub fn is_skill_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}
pub fn configure(
    tools: &mut Vec<ToolDefinition>,
    prompt: &mut Option<String>,
    data: &ProjectSkills,
    context_limit: Option<usize>,
) {
    tools.retain(|tool| !is_skill_tool(&tool.name));
    if !data.enabled {
        return;
    }
    tools.extend(TOOL_NAMES.iter().map(|name| definition(name)));
    let limit = context_limit.unwrap_or(32768);
    let fixed = openwebide_core::context::tool_schema_tokens(tools)
        .saturating_add(prompt.as_ref().map_or(0, |text| text.len().div_ceil(3)));
    // Leave room for replies, compaction and the user's message at small ceilings.
    // If there is no catalog space, skill_list still provides discovery on demand.
    let budget = (limit.saturating_mul(3) / 10).min(
        limit
            .saturating_sub(
                fixed
                    .saturating_add(limit / 4)
                    .saturating_add(limit / 8)
                    // Startup environment, task definition and current prompt.
                    .saturating_add(1024),
            )
            .saturating_mul(3),
    );
    if let Some(context) = openwebide_core::skills::skills_context(data, budget) {
        prompt
            .get_or_insert_with(String::new)
            .push_str(&format!("\n\n{context}"));
    }
}
pub fn definition(name: &str) -> ToolDefinition {
    let draft = serde_json::json!({"type":"object","properties":{
        "name":{"type":"string","description":"Lowercase kebab-case"},
        "description":{"type":"string"},"instructions":{"type":"string"},"enabled":{"type":"boolean"},
        "metadata":{"type":"object"},"resources":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"content":{"type":"string"},"binary":{"type":"boolean"}},"required":["name","content"],"additionalProperties":false}}
    },"required":["name","description","instructions"],"additionalProperties":false});
    let (description, parameters) = match name {
        "skill_list" => (
            "List enabled skills, 20 per page. Follow next_offset.",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string"},"offset":{"type":"integer"}},"additionalProperties":false}),
        ),
        "skill_read" => (
            "Read skill instructions or named resource. Follow next_offset. Defaults to 4000 characters; maximum 8000. User directions and approvals take precedence.",
            serde_json::json!({"type":"object","properties":{"id":{"type":"integer"},"resource":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["id"],"additionalProperties":false}),
        ),
        "skill_create" => (
            "Save a reusable skill; read the skill-authoring skill for its design workflow. Resources: relative paths, text or base64 when binary=true. Never executes resources.",
            serde_json::json!({"type":"object","properties":{"draft":draft},"required":["draft"],"additionalProperties":false}),
        ),
        "skill_update" => (
            "Update at the read revision; reread after conflicts. Omitted enabled/resources/metadata are preserved. Preserve name unless asked to rename.",
            serde_json::json!({"type":"object","properties":{"id":{"type":"integer"},"revision":{"type":"integer"},"draft":draft},"required":["id","revision","draft"],"additionalProperties":false}),
        ),
        "skill_creator" => (
            "Get an authoring/evaluation workflow for a goal or existing skill. Continue with the user and CRUD tools; does not evaluate or save automatically.",
            serde_json::json!({"type":"object","properties":{"goal":{"type":"string"},"id":{"type":"integer"}},"required":["goal"],"additionalProperties":false}),
        ),
        _ => (
            "Delete a skill at its read revision.",
            serde_json::json!({"type":"object","properties":{"id":{"type":"integer"},"revision":{"type":"integer"}},"required":["id","revision"],"additionalProperties":false}),
        ),
    };
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub id: i64,
    pub resource: Option<String>,
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatorArgs {
    pub goal: String,
    pub id: Option<i64>,
}
impl CreatorArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.goal.trim().is_empty()
            || self.goal.chars().count() > 4000
            || self.id.is_some_and(|id| id <= 0)
        {
            return Err("Provide a goal of 1–4000 characters and a valid skill ID".into());
        }
        Ok(())
    }
}
/// Adapted workflow guidance; no Claude CLI dependency or automatic external calls.
pub const CREATOR_MIGRATION_NOTICE: &str = "Install the Skill Authoring plugin and read its skill-authoring skill with skill_read for the authoring and evaluation workflow.";
pub trait SkillStore: Send + Sync {
    fn execute(
        &self,
        command: &SkillCommand,
    ) -> impl Future<Output = Result<ProjectSkills, String>> + Send;
}
pub struct SkillTools<E, S> {
    executor: E,
    store: S,
}
impl<E, S> SkillTools<E, S> {
    pub const fn new(executor: E, store: S) -> Self {
        Self { executor, store }
    }
}
fn summary(entry: &openwebide_core::ProjectSkill) -> serde_json::Value {
    serde_json::json!({"id":entry.id,"revision":entry.revision,"name":entry.draft.name,"description":entry.draft.description,"enabled":entry.draft.enabled})
}
fn text_page(text: &str, offset: usize, limit: usize) -> serde_json::Value {
    let total = text.chars().count();
    let content = text.chars().skip(offset).take(limit).collect::<String>();
    let end = offset.saturating_add(content.chars().count());
    serde_json::json!({"content": content, "offset": offset, "next_offset": (end < total).then_some(end), "total_characters": total})
}
fn instructions(
    entry: &openwebide_core::ProjectSkill,
    offset: usize,
    limit: usize,
) -> serde_json::Value {
    let mut value = summary(entry);
    value["instructions"] = text_page(&entry.draft.instructions, offset, limit);
    value["resources"] = serde_json::json!(
        entry
            .draft
            .resources
            .iter()
            .map(|resource| &resource.name)
            .collect::<Vec<_>>()
    );
    value
}
impl<E, S: SkillStore> SkillTools<E, S> {
    async fn preserve_fields(
        &self,
        call: &ToolCall,
        mut command: SkillCommand,
    ) -> Result<SkillCommand, String> {
        if let SkillCommand::Update {
            id,
            revision,
            draft,
        } = &mut command
        {
            let data = self
                .store
                .execute(&SkillCommand::Read {
                    id: *id,
                    resource: None,
                })
                .await?;
            let entry = data
                .entries
                .iter()
                .find(|entry| entry.id == *id)
                .ok_or("Skill no longer exists")?;
            if entry.revision != *revision {
                return Err("Skill changed. Read it again before editing.".into());
            }
            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).map_err(|error| error.to_string())?;
            let supplied = args["draft"].as_object().ok_or("Provide a skill draft")?;
            if !supplied.contains_key("resources") {
                draft.resources.clone_from(&entry.draft.resources);
            }
            if !supplied.contains_key("metadata") {
                draft.metadata.clone_from(&entry.draft.metadata);
            }
            if !supplied.contains_key("enabled") {
                draft.enabled = entry.draft.enabled;
            }
        }
        Ok(command)
    }
}
impl<E: ToolExecutor + Sync, S: SkillStore> ToolExecutor for SkillTools<E, S> {
    fn has_context(&self) -> bool {
        self.executor.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.executor.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        if is_skill_tool(&call.name) {
            crate::tools::parse(call).map_or_else(|e| e.to_string(), |tool| tool.describe())
        } else {
            self.executor.describe(call)
        }
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        if !is_skill_tool(&call.name) {
            return self.executor.preview(call).await;
        }
        let note = async {
            let crate::tools::Tool::Skill(command) =
                crate::tools::parse(call).map_err(|e| e.to_string())?
            else {
                return Ok(None);
            };
            let command = self.preserve_fields(call, command).await?;
            command.validate()?;
            match command {
                SkillCommand::Create { draft } => Ok(Some(
                    serde_json::to_string_pretty(&draft).map_err(|e| e.to_string())?,
                )),
                SkillCommand::Update {
                    id,
                    revision,
                    draft,
                } => {
                    let data = self
                        .store
                        .execute(&SkillCommand::Read { id, resource: None })
                        .await?;
                    let entry = data.entries.first().ok_or("Skill no longer exists")?;
                    if entry.revision != revision {
                        return Err("Skill changed. Read it again before editing.".into());
                    }
                    Ok(Some(format!(
                        "Replace skill {}:\n{}",
                        entry.draft.name,
                        serde_json::to_string_pretty(&draft).map_err(|e| e.to_string())?
                    )))
                }
                SkillCommand::Delete { id, revision } => {
                    let data = self
                        .store
                        .execute(&SkillCommand::Read { id, resource: None })
                        .await?;
                    let entry = data.entries.first().ok_or("Skill no longer exists")?;
                    if entry.revision != revision {
                        return Err("Skill changed. Read it again before deleting.".into());
                    }
                    Ok(Some(format!(
                        "Delete skill {}\n{}",
                        entry.draft.name, entry.draft.description
                    )))
                }
                _ => Ok(None),
            }
        }
        .await;
        match note {
            Ok(None) => None,
            Ok(Some(note)) | Err(note) => Some(ToolPreview {
                diff: None,
                note: Some(note),
            }),
        }
    }
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<FileDiff>, String> {
        self.executor.checkpoint(call).await
    }
    async fn project_checkpoint(&self, call: &ToolCall) -> Result<Option<ProjectSnapshot>, String> {
        self.executor.project_checkpoint(call).await
    }
    fn acknowledge(&self, id: &str) {
        self.executor.acknowledge(id);
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        if !is_skill_tool(&call.name) {
            return self.executor.execute(call).await;
        }
        let result = async {
            let parsed = crate::tools::parse(call).map_err(|e| e.to_string())?;
            if let crate::tools::Tool::SkillCreator(args) = parsed {
                args.validate()?;
                let data = self.store.execute(&SkillCommand::List { query: String::new() }).await?;
                let existing = if let Some(id) = args.id {
                    let entry = data.entries.iter().find(|entry| entry.id == id).ok_or("Enabled skill not found")?;
                    Some(instructions(entry, 0, 4000))
                } else { None };
                return Ok(serde_json::json!({"goal":args.goal,"existing_skill":existing,"workflow":CREATOR_MIGRATION_NOTICE}).to_string());
            }
            if let crate::tools::Tool::SkillList(args) = &parsed {
                if args.offset > openwebide_core::skills::MAX_SKILLS { return Err("Invalid skill list offset".into()); }
                let command = SkillCommand::List { query: args.query.clone() };
                let command = self.preserve_fields(call, command).await?;
            command.validate()?;
                let data = self.store.execute(&command).await?;
                let total = data.entries.len();
                let entries = data.entries.iter().filter(|entry| entry.draft.enabled).skip(args.offset).take(20).map(|entry| {
                    let mut value = summary(entry);
                    value["description"] = serde_json::json!(entry.draft.description.chars().take(256).collect::<String>());
                    value["description_truncated"] = serde_json::json!(entry.draft.description.chars().count() > 256);
                    value
                }).collect::<Vec<_>>();
                let end = args.offset + entries.len();
                return Ok(serde_json::json!({"entries": entries, "total":total, "next_offset": (end < total).then_some(end)}).to_string());
            }
            if let crate::tools::Tool::SkillRead(args) = &parsed {
                let limit = args.limit.unwrap_or(4000);
                if args.offset > openwebide_core::skills::MAX_INSTRUCTIONS || !(1..=8000).contains(&limit) { return Err("Invalid skill read window".into()); }
                let command = SkillCommand::Read { id: args.id, resource: args.resource.clone() };
                let command = self.preserve_fields(call, command).await?;
            command.validate()?;
                let data = self.store.execute(&command).await?;
                let entry = data.entries.iter().find(|entry| entry.id == args.id).ok_or("Skill not found")?;
                let value = if let Some(name) = &args.resource {
                    let resource = entry.draft.resources.iter().find(|resource| &resource.name == name).ok_or("Skill resource not found")?;
                    let mut value = text_page(&resource.content, args.offset, limit);
                    value["name"] = serde_json::json!(name); value["binary"] = serde_json::json!(resource.binary);
                    value
                } else { instructions(entry, args.offset, limit) };
                return Ok(value.to_string());
            }
            let crate::tools::Tool::Skill(command) = parsed else { return Err("Invalid skill tool".into()); };
            let command = self.preserve_fields(call, command).await?;
            command.validate()?;
            let data = self.store.execute(&command).await?;
            let value = match command {
                SkillCommand::List { .. } => serde_json::json!(data.entries.iter().filter(|entry| entry.draft.enabled).map(summary).collect::<Vec<_>>()),
                SkillCommand::Read { id, resource } => {
                    let entry = data.entries.iter().find(|entry| entry.id == id).ok_or("Skill not found")?;
                    if let Some(name) = resource { serde_json::json!(entry.draft.resources.iter().find(|resource| resource.name == name).ok_or("Skill resource not found")?) } else { instructions(entry, 0, 4000) }
                }
                SkillCommand::Create { draft } => summary(data.entries.iter().find(|entry| entry.draft.name == draft.name).ok_or("Created skill not found")?),
                SkillCommand::Update { id, .. } => summary(data.entries.iter().find(|entry| entry.id == id).ok_or("Updated skill not found")?),
                SkillCommand::Delete { id, .. } => serde_json::json!({"deleted":id}),
                SkillCommand::SetEnabled { .. } => return Err("Agents cannot toggle project skills".into()),
            };
            Ok(value.to_string())
        }.await;
        match result {
            Ok(content) => ToolOutcome {
                ok: true,
                content,
                summary: format!("{} complete", self.describe(call)),
                diff: None,
            },
            Err(content) => ToolOutcome {
                ok: false,
                summary: format!("Skill operation failed: {content}"),
                content,
                diff: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct ReadStore(ProjectSkills);
    impl SkillStore for ReadStore {
        async fn execute(&self, _: &SkillCommand) -> Result<ProjectSkills, String> {
            Ok(self.0.clone())
        }
    }
    fn fixture() -> SkillTools<crate::VfsToolExecutor<openwebide_core::MemoryVfs>, ReadStore> {
        let draft = openwebide_core::SkillDraft {
            name: "test-skill".into(),
            description: "d".repeat(1024),
            instructions: "🦀\u{0}".repeat(10000),
            enabled: true,
            resources: Vec::new(),
            metadata: Default::default(),
        };
        let data = ProjectSkills {
            enabled: true,
            entries: (1..=25)
                .map(|id| openwebide_core::ProjectSkill {
                    plugin: None,
                    id,
                    revision: 1,
                    updated_at: 0,
                    draft: draft.clone(),
                })
                .collect(),
        };
        SkillTools::new(
            crate::VfsToolExecutor::new(openwebide_core::MemoryVfs::new()),
            ReadStore(data),
        )
    }
    #[test]
    fn update_preserves_omitted_resources_metadata_and_enabled_with_revision_checks() {
        futures::executor::block_on(async {
            let mut executor = fixture();
            let entry = &mut executor.store.0.entries[0];
            entry.draft.resources.push(openwebide_core::SkillResource {
                name: "reference.txt".into(),
                content: "keep".into(),
                binary: false,
            });
            entry
                .draft
                .metadata
                .insert("license".into(), serde_json::json!("MIT"));
            entry.draft.enabled = false;
            let call = ToolCall { id: "test".into(), name: "skill_update".into(), arguments: serde_json::json!({"id":1,"revision":1,"draft":{"name":"test-skill","description":"Updated","instructions":"New"}}).to_string() };
            let crate::tools::Tool::Skill(command) = crate::tools::parse(&call).unwrap() else {
                panic!("Expected skill command");
            };
            let SkillCommand::Update { draft, .. } = executor
                .preserve_fields(&call, command.clone())
                .await
                .unwrap()
            else {
                panic!("Expected update");
            };
            assert_eq!(draft.instructions, "New");
            assert_eq!(draft.resources, executor.store.0.entries[0].draft.resources);
            assert_eq!(draft.metadata, executor.store.0.entries[0].draft.metadata);
            assert!(!draft.enabled);
            executor.store.0.entries[0].revision = 2;
            assert!(executor.preserve_fields(&call, command).await.is_err());
        });
    }
    #[test]
    fn discovery_falls_back_to_list_when_fixed_context_leaves_no_catalog_room() {
        let executor = fixture();
        let mut tools = Vec::new();
        let original = "project instructions ".repeat(1000);
        let mut prompt = Some(original.clone());
        configure(&mut tools, &mut prompt, &executor.store.0, Some(4096));
        assert_eq!(prompt.as_deref(), Some(original.as_str()));
        assert!(tools.iter().any(|tool| tool.name == "skill_list"));
    }
    #[test]
    fn combined_tools_and_discovery_leave_compaction_room_at_eight_k() {
        let mut tools = crate::session::tools_for_host(true);
        let mut prompt = Some("Follow the user's project instructions.".into());
        crate::memory::configure(&mut tools, &mut prompt, &Default::default(), Some(8192));
        crate::scheduled::configure(&mut tools);
        configure(&mut tools, &mut prompt, &fixture().store.0, Some(8192));
        tools.push(crate::tasks::executor::definition());
        let mut request = openwebide_core::ChatRequest {
            connection_id: 1,
            model: None,
            system_prompt: prompt,
            messages: vec![openwebide_core::ChatMessage {
                id: 1,
                session_id: 1,
                role: openwebide_core::Role::User,
                content: "Inspect the project and explain the next steps.".into(),
                tool_calls: None,
                tool_call_id: None,
                created_at: 0,
                usage: None,
            }],
            tools,
            model_settings: Default::default(),
        };
        for mode in [
            openwebide_core::WorkspaceMode::Local,
            openwebide_core::WorkspaceMode::Remote,
        ] {
            let environment = openwebide_core::RunEnvironment {
                mode: Some(mode),
                project_name: Some("budget-test".into()),
                project_root: Some("sample".into()),
                browser_preferences: Some(openwebide_core::BrowserPreferences {
                    timezone: Some("America/Chicago".into()),
                    locale: Some("en-US".into()),
                    hour_cycle: Some("h12".into()),
                    utc_offset_minutes: Some(-360),
                }),
                ..Default::default()
            };
            let startup =
                futures::executor::block_on(crate::context::RunContext::new(environment).startup(
                    &openwebide_core::MemoryVfs::new(),
                    &crate::NoopBridgeClient,
                    &request.tools,
                ));
            request.messages.push(openwebide_core::ChatMessage {
                id: 2,
                session_id: 1,
                role: openwebide_core::Role::System,
                content: format!("[Open WebIDE run context]\n{startup}"),
                tool_calls: None,
                tool_call_id: None,
                created_at: 0,
                usage: None,
            });
            let tokens = openwebide_core::context::conservative_tokens(&request);
            assert!(
                tokens < 8192 - 2048 - 1024,
                "{mode:?}: {tokens} tokens leave insufficient compaction room"
            );
            request.messages.pop();
        }
        for name in [
            "ask_user_question",
            "monitor",
            "schedule_create",
            "skill_read",
            "task",
        ] {
            assert!(request.tools.iter().any(|tool| tool.name == name));
        }
    }
    #[test]
    fn skill_schemas_have_a_bounded_context_cost() {
        let tools = TOOL_NAMES
            .iter()
            .map(|name| definition(name))
            .collect::<Vec<_>>();
        assert!(openwebide_core::context::tool_schema_tokens(&tools) < 1400);
    }
    #[test]
    fn skill_read_and_list_pages_are_bounded_complete_and_strict() {
        futures::executor::block_on(async {
            let executor = fixture();
            let call = |name: &str, args: serde_json::Value| ToolCall {
                id: "test".into(),
                name: name.into(),
                arguments: args.to_string(),
            };
            let mut offset = 0;
            let mut text = String::new();
            loop {
                let result = executor
                    .execute(&call(
                        "skill_read",
                        serde_json::json!({"id":1,"offset":offset,"limit":8000}),
                    ))
                    .await;
                assert!(result.ok);
                assert!(result.content.len() < 64 * 1024);
                let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
                text.push_str(value["instructions"]["content"].as_str().unwrap());
                let Some(next) = value["instructions"]["next_offset"].as_u64() else {
                    break;
                };
                offset = next;
            }
            assert_eq!(text, "🦀\u{0}".repeat(10000));
            let result = executor
                .execute(&call("skill_list", serde_json::json!({})))
                .await;
            assert!(result.ok);
            assert!(result.content.len() < 64 * 1024);
            let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
            assert_eq!(value["entries"].as_array().unwrap().len(), 20);
            assert_eq!(value["next_offset"], 20);
            assert_eq!(value["entries"][0]["description_truncated"], true);
            let result = executor
                .execute(&call("skill_list", serde_json::json!({"offset":20})))
                .await;
            let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
            assert_eq!(value["entries"].as_array().unwrap().len(), 5);
            assert!(value["next_offset"].is_null());
            for args in [
                serde_json::json!({"id":1,"limit":0}),
                serde_json::json!({"id":1,"limit":8001}),
                serde_json::json!({"id":1,"offset":-1}),
                serde_json::json!({"id":1,"action":"delete"}),
            ] {
                assert!(!executor.execute(&call("skill_read", args)).await.ok);
            }
            assert!(
                !executor
                    .execute(&call("skill_list", serde_json::json!({"offset":101})))
                    .await
                    .ok
            );
        });
    }
}

/// Capture only active package contributions; personal skills remain live.
pub fn package_snapshot(data: &ProjectSkills) -> Vec<openwebide_core::ProjectSkill> {
    if !data.enabled {
        return Vec::new();
    }
    data.entries
        .iter()
        .filter(|entry| entry.draft.enabled && entry.plugin.is_some())
        .cloned()
        .collect()
}

/// Read-only access to the package contributions captured for a run. This loader
/// needs neither an authoring plugin nor live account persistence.
pub struct PackageSkillStore {
    entries: Vec<openwebide_core::ProjectSkill>,
}
impl PackageSkillStore {
    pub fn new(data: &ProjectSkills) -> Self {
        Self {
            entries: package_snapshot(data),
        }
    }
}
impl SkillStore for PackageSkillStore {
    async fn execute(&self, command: &SkillCommand) -> Result<ProjectSkills, String> {
        command.validate()?;
        let entries = match command {
            SkillCommand::List { query } => {
                let query = query.to_lowercase();
                self.entries
                    .iter()
                    .filter(|entry| {
                        entry.draft.name.to_lowercase().contains(&query)
                            || entry.draft.description.to_lowercase().contains(&query)
                    })
                    .cloned()
                    .collect()
            }
            SkillCommand::Read { id, resource } => {
                let entry = self
                    .entries
                    .iter()
                    .find(|entry| entry.id == *id)
                    .ok_or("This package skill was not enabled when the run started.")?;
                if resource.as_ref().is_some_and(|name| {
                    !entry
                        .draft
                        .resources
                        .iter()
                        .any(|asset| asset.name == *name)
                }) {
                    return Err("Skill resource not found".into());
                }
                vec![entry.clone()]
            }
            _ => {
                return Err("Package skill loading is read-only. Manage plugins in Plugins.".into());
            }
        };
        Ok(ProjectSkills {
            enabled: true,
            entries,
        })
    }
}

/// Package contributions are pinned once at run planning; user-authored skills stay live.
pub async fn pinned_command(
    command: &SkillCommand,
    pinned: &[openwebide_core::ProjectSkill],
    live: impl Future<Output = Result<ProjectSkills, String>>,
) -> Result<ProjectSkills, String> {
    command.validate()?;
    if let SkillCommand::Read { id, resource } = command
        && let Some(entry) = pinned.iter().find(|entry| entry.id == *id)
    {
        if resource.as_ref().is_some_and(|name| {
            !entry
                .draft
                .resources
                .iter()
                .any(|asset| asset.name == *name)
        }) {
            return Err("Skill resource not found".into());
        }
        return Ok(ProjectSkills {
            enabled: true,
            entries: vec![entry.clone()],
        });
    }
    if let SkillCommand::Update { id, .. } | SkillCommand::Delete { id, .. } = command
        && pinned.iter().any(|entry| entry.id == *id)
    {
        return Err("Manage package skills through Settings → Plugins.".into());
    }
    let mut result = live.await?;
    match command {
        SkillCommand::List { query } => {
            result.entries.retain(|entry| entry.plugin.is_none());
            let query = query.to_lowercase();
            let mut entries = pinned
                .iter()
                .filter(|entry| {
                    entry.draft.name.to_lowercase().contains(&query)
                        || entry.draft.description.to_lowercase().contains(&query)
                })
                .cloned()
                .collect::<Vec<_>>();
            entries.extend(result.entries);
            entries.truncate(openwebide_core::skills::MAX_SKILLS);
            entries.sort_by(|a, b| a.draft.name.cmp(&b.draft.name).then(a.id.cmp(&b.id)));
            result.entries = entries;
        }
        SkillCommand::Read { .. } if result.entries.iter().any(|entry| entry.plugin.is_some()) => {
            return Err("This package skill was not enabled when the run started.".into());
        }
        _ => {}
    }
    Ok(result)
}

#[cfg(test)]
mod plugin_run_tests {
    use super::*;
    use openwebide_core::{ProjectSkill, SkillDraft, SkillResource, plugins::PluginSkillOrigin};
    fn entry(id: i64, managed: bool) -> ProjectSkill {
        ProjectSkill {
            id,
            revision: 1,
            updated_at: 0,
            plugin: managed.then(|| PluginSkillOrigin {
                publisher: "openwebide".into(),
                name: "pr-review".into(),
                version: "0.1.0".into(),
                commit: "a".repeat(40),
            }),
            draft: SkillDraft {
                name: format!("skill-{id}"),
                description: "Review changes".into(),
                instructions: "Original instructions".into(),
                enabled: true,
                resources: vec![SkillResource {
                    name: "references/check.md".into(),
                    content: "Original resource".into(),
                    binary: false,
                }],
                metadata: Default::default(),
            },
        }
    }
    #[test]
    fn package_loader_is_independent_read_only_and_pinned() {
        futures::executor::block_on(async {
            let mut disabled = entry(2, true);
            disabled.draft.enabled = false;
            let mut data = ProjectSkills {
                enabled: true,
                entries: vec![entry(1, true), disabled, entry(3, false)],
            };
            let store = PackageSkillStore::new(&data);
            data.entries.clear();
            data.enabled = false;
            let list = store
                .execute(&SkillCommand::List {
                    query: "REVIEW".into(),
                })
                .await
                .unwrap();
            assert_eq!(list.entries, vec![entry(1, true)]);
            let read = store
                .execute(&SkillCommand::Read {
                    id: 1,
                    resource: Some("references/check.md".into()),
                })
                .await
                .unwrap();
            assert_eq!(read.entries, list.entries);
            for command in [
                SkillCommand::Read {
                    id: 2,
                    resource: None,
                },
                SkillCommand::Read {
                    id: 3,
                    resource: None,
                },
                SkillCommand::Read {
                    id: 1,
                    resource: Some("missing.md".into()),
                },
                SkillCommand::Create {
                    draft: entry(4, false).draft,
                },
                SkillCommand::Update {
                    id: 1,
                    revision: 1,
                    draft: entry(1, true).draft,
                },
                SkillCommand::Delete { id: 1, revision: 1 },
            ] {
                assert!(store.execute(&command).await.is_err(), "{command:?}");
            }
            let disabled = PackageSkillStore::new(&data);
            assert!(
                disabled
                    .execute(&SkillCommand::List {
                        query: String::new()
                    })
                    .await
                    .unwrap()
                    .entries
                    .is_empty()
            );
        });
    }
    #[test]
    fn running_package_reads_remain_pinned_after_update_disable_or_uninstall() {
        futures::executor::block_on(async {
            let pinned = vec![entry(1, true)];
            for resource in [None, Some("references/check.md".into())] {
                let result =
                    pinned_command(&SkillCommand::Read { id: 1, resource }, &pinned, async {
                        panic!("Pinned read must not consult live storage")
                    })
                    .await
                    .unwrap();
                assert_eq!(result.entries, pinned);
            }
            assert!(
                pinned_command(
                    &SkillCommand::Read {
                        id: 1,
                        resource: Some("missing.md".into())
                    },
                    &pinned,
                    async { panic!("Unexpected live read") }
                )
                .await
                .is_err()
            );
            for command in [
                SkillCommand::Delete { id: 1, revision: 1 },
                SkillCommand::Update {
                    id: 1,
                    revision: 1,
                    draft: pinned[0].draft.clone(),
                },
            ] {
                assert!(
                    pinned_command(&command, &pinned, async {
                        panic!("Package edit must be rejected")
                    })
                    .await
                    .is_err()
                );
            }
        });
    }
    #[test]
    fn running_catalog_excludes_new_packages_and_keeps_personal_skills_live() {
        futures::executor::block_on(async {
            let pinned = vec![entry(1, true)];
            let mut updated = entry(1, true);
            updated.draft.instructions = "New package instructions".into();
            let mut personal = entry(3, false);
            personal.draft.instructions = "Current personal instructions".into();
            let live = ProjectSkills {
                enabled: true,
                entries: vec![updated, entry(2, true), personal.clone()],
            };
            let list = pinned_command(
                &SkillCommand::List {
                    query: String::new(),
                },
                &pinned,
                async { Ok(live.clone()) },
            )
            .await
            .unwrap();
            assert_eq!(list.entries, vec![pinned[0].clone(), personal.clone()]);
            assert!(
                pinned_command(
                    &SkillCommand::Read {
                        id: 2,
                        resource: None
                    },
                    &pinned,
                    async {
                        Ok(ProjectSkills {
                            enabled: true,
                            entries: vec![entry(2, true)],
                        })
                    }
                )
                .await
                .is_err()
            );
            let read = pinned_command(
                &SkillCommand::Read {
                    id: 3,
                    resource: None,
                },
                &pinned,
                async {
                    Ok(ProjectSkills {
                        enabled: true,
                        entries: vec![personal.clone()],
                    })
                },
            )
            .await
            .unwrap();
            assert_eq!(read.entries, vec![personal]);
            let list = pinned_command(
                &SkillCommand::List {
                    query: "skill-1".into(),
                },
                &pinned,
                async {
                    Ok(ProjectSkills {
                        enabled: true,
                        entries: Vec::new(),
                    })
                },
            )
            .await
            .unwrap();
            assert_eq!(list.entries, pinned);
        });
    }
}
