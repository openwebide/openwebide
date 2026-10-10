//! One contribution policy for local and remote run preparation.
pub mod actions;
pub mod completion;
pub mod execution;
pub mod jobs;
pub mod runs;
mod worker;
use openwebide_core::{ToolDefinition, plugins::ProjectPlugin};
/// Keep workspace primitives built in; optional services are supplied by plugins.
pub struct PluginContext<'a> {
    pub bindings: &'a [ProjectPlugin],
    pub memories: &'a openwebide_core::ProjectMemories,
    pub skills: &'a openwebide_core::ProjectSkills,
    pub context_limit: Option<usize>,
}
pub fn configure(
    tools: &mut Vec<ToolDefinition>,
    prompt: &mut Option<String>,
    context: &PluginContext<'_>,
) {
    tools.retain(|tool| {
        !crate::memory::is_memory_tool(&tool.name)
            && !crate::scheduled::is_scheduled_tool(&tool.name)
            && !crate::skills::is_skill_tool(&tool.name)
            && !matches!(tool.name.as_str(), "search_web" | "fetch_web_page")
    });
    let sdk_skill_reader = context.bindings.iter().any(|plugin| {
        plugin.enabled
            && plugin.prepared.manifest.executable.is_some()
            && plugin
                .prepared
                .manifest
                .contributions
                .tools
                .iter()
                .any(|tool| tool.name == "skill_read")
            && plugin
                .prepared
                .manifest
                .contributions
                .tools
                .iter()
                .any(|tool| tool.name == "skill_list")
    });
    if !sdk_skill_reader {
        crate::skills::packages::configure(tools, prompt, context.skills, context.context_limit);
    } else {
        tools.retain(|tool| !crate::skills::packages::TOOL_NAMES.contains(&tool.name.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::plugins::PluginToolGroup;
    #[test]
    fn sdk_skill_handlers_replace_legacy_tools_without_builtin_catalog_policy() {
        for mut tools in [crate::vfs_tools(), crate::session::tools_for_host(true)] {
            let mut prepared = openwebide_core::plugins::testing::receipt();
            prepared.manifest.compatibility.plugin_api = 3;
            prepared.manifest.contributions.skills.clear();
            prepared.manifest.contributions.tools = crate::skills::TOOL_NAMES
                .iter()
                .map(|name| {
                    let definition = crate::skills::definition(name);
                    openwebide_core::plugins::PluginTool {
                        name: definition.name,
                        description: definition.description,
                        parameters: definition.parameters,
                        requires_approval: matches!(
                            *name,
                            "skill_create" | "skill_update" | "skill_delete"
                        ),
                    }
                })
                .collect();
            prepared.manifest.executable = Some(openwebide_core::plugins::RustPlugin {
                manifest: "Cargo.toml".into(),
                library: "community_skills".into(),
                sdk_version: "0.1.0".into(),
                capabilities: vec!["collections".into()],
            });
            let bindings = [ProjectPlugin {
                id: 1,
                revision: 1,
                prepared,
                enabled: true,
            }];
            let skills = openwebide_core::ProjectSkills {
                enabled: true,
                entries: vec![openwebide_core::ProjectSkill {
                    plugin: None,
                    id: 1,
                    revision: 1,
                    updated_at: 0,
                    draft: openwebide_core::SkillDraft {
                        name: "personal-guide".into(),
                        description: "PRIVATE BUILTIN CATALOG".into(),
                        instructions: "Guide".into(),
                        enabled: true,
                        resources: Vec::new(),
                        metadata: Default::default(),
                    },
                }],
            };
            let memories = openwebide_core::ProjectMemories {
                enabled: true,
                entries: Vec::new(),
            };
            let mut prompt = None;
            configure(
                &mut tools,
                &mut prompt,
                &PluginContext {
                    bindings: &bindings,
                    memories: &memories,
                    skills: &skills,
                    context_limit: None,
                },
            );
            assert!(
                prompt.is_none(),
                "SDK context owns the catalog contribution"
            );
            let executables = execution::configure_tools(&mut tools, &bindings).unwrap();
            assert_eq!(executables.len(), 1);
            for name in crate::skills::TOOL_NAMES {
                assert_eq!(tools.iter().filter(|tool| tool.name == *name).count(), 1);
            }
        }
    }
    #[test]
    fn legacy_tool_groups_cannot_activate_builtin_behavior_in_both_host_modes() {
        for mut tools in [crate::vfs_tools(), crate::session::tools_for_host(true)] {
            let memories = openwebide_core::ProjectMemories {
                enabled: true,
                entries: vec![openwebide_core::ProjectMemory {
                    auto_title: false,
                    id: 1,
                    title: "Fact".into(),
                    content: "PRIVATE MEMORY".into(),
                    revision: 1,
                    updated_at: 0,
                }],
            };
            let skills = openwebide_core::ProjectSkills::default();
            let mut prompt = None;
            configure(
                &mut tools,
                &mut prompt,
                &PluginContext {
                    bindings: &[],
                    memories: &memories,
                    skills: &skills,
                    context_limit: None,
                },
            );
            assert!(
                !prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.contains("PRIVATE MEMORY"))
            );
            assert!(tools.iter().any(|tool| tool.name == "read_file"));
            assert!(tools.iter().any(|tool| tool.name == "run_command"));
            assert!(!tools.iter().any(|tool| matches!(
                tool.name.as_str(),
                "search_web"
                    | "schedule_list"
                    | "memory_read"
                    | "skill_create"
                    | "skill_creator"
                    | "skill_read"
                    | "skill_list"
            )));
            let mut prepared = openwebide_core::plugins::testing::receipt();
            prepared.manifest.compatibility.plugin_api = 2;
            prepared.manifest.contributions.tool_groups = vec![
                PluginToolGroup::Web,
                PluginToolGroup::Memory,
                PluginToolGroup::Scheduling,
                PluginToolGroup::SkillAuthoring,
            ];
            let mut bindings = vec![ProjectPlugin {
                id: 1,
                revision: 1,
                prepared,
                enabled: true,
            }];
            tools = crate::vfs_tools();
            configure(
                &mut tools,
                &mut prompt,
                &PluginContext {
                    bindings: &bindings,
                    memories: &memories,
                    skills: &skills,
                    context_limit: None,
                },
            );
            for name in [
                "search_web",
                "fetch_web_page",
                "memory_read",
                "schedule_list",
                "monitor",
                "skill_create",
                "skill_read",
            ] {
                assert!(
                    !tools.iter().any(|tool| tool.name == name),
                    "legacy tool {name}"
                );
            }
            assert!(!tools.iter().any(|tool| tool.name == "skill_creator"));
            assert!(
                !prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.contains("PRIVATE MEMORY"))
            );
            bindings[0].enabled = false;
            prompt = None;
            configure(
                &mut tools,
                &mut prompt,
                &PluginContext {
                    bindings: &bindings,
                    memories: &memories,
                    skills: &skills,
                    context_limit: None,
                },
            );
            assert!(!tools.iter().any(|tool| matches!(
                tool.name.as_str(),
                "search_web" | "memory_read" | "monitor" | "skill_create"
            )));
        }
    }
}
