//! Generic progressive loading of pinned plugin skills, independent of authoring.
use super::{PackageSkillStore, SkillTools};
use crate::{ToolExecutor, ToolOutcome, ToolPreview};
use openwebide_core::{FileDiff, ToolCall, ToolDefinition, rewind::ProjectSnapshot};

pub const TOOL_NAMES: &[&str] = &["plugin_skill_list", "plugin_skill_read"];

pub fn configure(
    tools: &mut Vec<ToolDefinition>,
    prompt: &mut Option<String>,
    skills: &openwebide_core::ProjectSkills,
    context_limit: Option<usize>,
) {
    tools.retain(|tool| !TOOL_NAMES.contains(&tool.name.as_str()));
    let entries = super::package_snapshot(skills);
    if entries.is_empty() {
        return;
    }
    for (name, source) in TOOL_NAMES.iter().zip(["skill_list", "skill_read"]) {
        let mut definition = super::definition(source);
        definition.name = (*name).into();
        tools.push(definition);
    }
    super::append_catalog(
        tools,
        prompt,
        &openwebide_core::ProjectSkills {
            enabled: true,
            entries,
        },
        context_limit,
        true,
    );
}

pub struct PackageSkillTools<E> {
    inner: SkillTools<E, PackageSkillStore>,
}
impl<E> PackageSkillTools<E> {
    pub fn new(executor: E, entries: &[openwebide_core::ProjectSkill]) -> Self {
        Self {
            inner: SkillTools::new(
                executor,
                PackageSkillStore::new(&openwebide_core::ProjectSkills {
                    enabled: true,
                    entries: entries.to_vec(),
                }),
            ),
        }
    }
}
fn reader_call(call: &ToolCall) -> Option<ToolCall> {
    let name = match call.name.as_str() {
        "plugin_skill_list" => "skill_list",
        "plugin_skill_read" => "skill_read",
        _ => return None,
    };
    let mut call = call.clone();
    call.name = name.into();
    Some(call)
}
impl<E: ToolExecutor + Sync> ToolExecutor for PackageSkillTools<E> {
    fn has_context(&self) -> bool {
        self.inner.executor.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.inner.executor.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        reader_call(call).map_or_else(
            || self.inner.executor.describe(call),
            |call| self.inner.describe(&call),
        )
    }
    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        if reader_call(call).is_some() {
            None
        } else {
            self.inner.executor.preview(call).await
        }
    }
    async fn checkpoint(&self, call: &ToolCall) -> Result<Option<FileDiff>, String> {
        if reader_call(call).is_some() {
            Ok(None)
        } else {
            self.inner.executor.checkpoint(call).await
        }
    }
    async fn project_checkpoint(&self, call: &ToolCall) -> Result<Option<ProjectSnapshot>, String> {
        if reader_call(call).is_some() {
            Ok(None)
        } else {
            self.inner.executor.project_checkpoint(call).await
        }
    }
    fn acknowledge(&self, id: &str) {
        self.inner.executor.acknowledge(id);
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutcome {
        if let Some(call) = reader_call(call) {
            self.inner.execute(&call).await
        } else {
            self.inner.executor.execute(call).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn skills() -> openwebide_core::ProjectSkills {
        serde_json::from_value(serde_json::json!({
            "enabled": true, "entries": [{
                "id": 1, "revision": 1, "updated_at": 0,
                "plugin": {"publisher":"community", "name":"review", "version":"1.0.0", "commit":"a".repeat(40)},
                "draft": {"name":"review", "description":"Review changes", "instructions":"Pinned instructions",
                    "resources":[{"name":"guide.md", "content":"Pinned resource", "binary":false}]}
            }]
        })).unwrap()
    }
    struct Executor;
    impl ToolExecutor for Executor {
        fn describe(&self, call: &ToolCall) -> String {
            call.name.clone()
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutcome {
            ToolOutcome {
                ok: true,
                content: call.name.clone(),
                summary: "Delegated".into(),
                diff: None,
            }
        }
    }
    #[test]
    fn fallback_catalog_and_execution_work_without_authoring_in_both_modes() {
        futures::executor::block_on(async {
            for mut tools in [crate::vfs_tools(), crate::session::tools_for_host(true)] {
                let data = skills();
                let mut prompt = None;
                crate::plugins::configure(
                    &mut tools,
                    &mut prompt,
                    &crate::plugins::PluginContext {
                        bindings: &[],
                        memories: &Default::default(),
                        skills: &data,
                        context_limit: None,
                    },
                );
                assert!(prompt.unwrap().contains("plugin_skill_read"));
                assert!(tools.iter().any(|tool| tool.name == "plugin_skill_read"));
                assert!(!tools.iter().any(|tool| tool.name == "skill_create"));
                let executor = PackageSkillTools::new(Executor, &data.entries);
                for (name, arguments) in [
                    ("plugin_skill_list", serde_json::json!({"query":"REVIEW"})),
                    ("plugin_skill_read", serde_json::json!({"id":1})),
                    (
                        "plugin_skill_read",
                        serde_json::json!({"id":1,"resource":"guide.md"}),
                    ),
                ] {
                    let call = ToolCall {
                        id: "test".into(),
                        name: name.into(),
                        arguments: arguments.to_string(),
                    };
                    assert!(!crate::policy::requires_approval(&call));
                    assert!(crate::session::is_projectless_tool(name));
                    let outcome = executor.execute(&call).await;
                    assert!(outcome.ok, "{}", outcome.content);
                    assert!(outcome.content.contains(if name == "plugin_skill_list" {
                        "Review changes"
                    } else {
                        "Pinned"
                    }));
                }
                let call = ToolCall {
                    id: "delegate".into(),
                    name: "skill_read".into(),
                    arguments: "{}".into(),
                };
                assert_eq!(executor.execute(&call).await.content, "skill_read");
                let call = ToolCall {
                    id: "missing".into(),
                    name: "plugin_skill_read".into(),
                    arguments: "{\"id\":2}".into(),
                };
                assert!(!executor.execute(&call).await.ok);
                let mut disabled = data;
                disabled.enabled = false;
                let mut prompt = None;
                crate::plugins::configure(
                    &mut tools,
                    &mut prompt,
                    &crate::plugins::PluginContext {
                        bindings: &[],
                        memories: &Default::default(),
                        skills: &disabled,
                        context_limit: None,
                    },
                );
                assert!(prompt.is_none());
                assert!(
                    !tools
                        .iter()
                        .any(|tool| TOOL_NAMES.contains(&tool.name.as_str()))
                );
            }
        });
    }
    #[test]
    fn partial_executable_skill_handlers_do_not_hide_package_discovery() {
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 3;
        prepared.manifest.contributions.tool_groups.clear();
        prepared.manifest.contributions.tools = vec![openwebide_core::plugins::PluginTool {
            name: "skill_read".into(),
            description: "Read custom skills".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        }];
        prepared.manifest.executable = Some(openwebide_core::plugins::RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "custom_skills".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["collections".into()],
        });
        let mut bindings = vec![openwebide_core::plugins::ProjectPlugin {
            id: 1,
            revision: 1,
            prepared,
            enabled: true,
        }];
        let data = skills();
        for has_list in [false, true] {
            let mut tools = crate::vfs_tools();
            let mut prompt = None;
            crate::plugins::configure(
                &mut tools,
                &mut prompt,
                &crate::plugins::PluginContext {
                    bindings: &bindings,
                    memories: &Default::default(),
                    skills: &data,
                    context_limit: None,
                },
            );
            assert_eq!(
                tools.iter().any(|tool| tool.name == "plugin_skill_list"),
                !has_list
            );
            assert_eq!(prompt.is_some(), !has_list);
            bindings[0].prepared.manifest.contributions.tools.push(
                openwebide_core::plugins::PluginTool {
                    name: "skill_list".into(),
                    description: "Discover custom skills".into(),
                    parameters: serde_json::json!({"type":"object"}),
                    requires_approval: false,
                },
            );
        }
    }
}
