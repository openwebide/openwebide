//! Agent language-query envelopes. Account/session authority comes from the
//! runtime, never model arguments; all operations are read-only.
use super::LanguagePreferences;
use crate::plugins::ProjectPlugin;
use crate::{Project, RunEnvironment, WorkspaceMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageToolScope {
    pub project: i64,
    pub mode: WorkspaceMode,
    pub root: Option<String>,
}
impl LanguageToolScope {
    pub fn for_project(project: &Project) -> Self {
        Self {
            project: project.id,
            mode: project.mode,
            root: crate::run::execution_root(project),
        }
    }
    pub fn from_environment(environment: &RunEnvironment) -> Option<Self> {
        let mode = environment.mode?;
        Some(Self {
            project: environment.project_id?,
            mode,
            root: match mode {
                WorkspaceMode::Remote => environment.project_root.clone(),
                WorkspaceMode::Local => None,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageToolContext {
    pub scope: LanguageToolScope,
    pub bindings: Vec<ProjectPlugin>,
    pub preferences: LanguagePreferences,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageToolOperation {
    Diagnostics,
    Symbols,
    WorkspaceSymbols,
    Definitions,
    References,
    ResolveSymbol,
    ReadResource,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageToolArgs {
    pub operation: LanguageToolOperation,
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub query: Option<String>,
    pub id: Option<String>,
    /// Read-only external resource line window, using the read_file convention.
    pub offset: Option<u32>,
    pub limit: Option<u32>,
}
impl LanguageToolArgs {
    pub fn query(&self) -> Result<super::intelligence::Query, String> {
        use super::intelligence::{Position, Query};
        let navigation = matches!(
            self.operation,
            LanguageToolOperation::Definitions | LanguageToolOperation::References
        );
        let resource = self.operation == LanguageToolOperation::ReadResource;
        let identity = resource || self.operation == LanguageToolOperation::ResolveSymbol;
        if !super::intelligence::relative_path(&self.path)
            || self
                .query
                .as_ref()
                .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
            || self.id.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            })
            || self.line == Some(0)
            || self.column == Some(0)
            || (!navigation && (self.line.is_some() || self.column.is_some()))
            || (!resource && (self.offset.is_some() || self.limit.is_some()))
            || self.offset == Some(0)
            || self.limit == Some(0)
            || self.limit.is_some_and(|limit| limit > 2000)
            || (!identity && self.id.is_some())
            || (self.operation != LanguageToolOperation::WorkspaceSymbols && self.query.is_some())
        {
            return Err("Invalid code-intelligence path, position or query".into());
        }
        let position = || {
            Ok::<_, String>(Position {
                line: self
                    .line
                    .and_then(|line| line.checked_sub(1))
                    .ok_or("A navigation query requires a 1-based line")?,
                character: self
                    .column
                    .and_then(|column| column.checked_sub(1))
                    .ok_or("A navigation query requires a 1-based UTF-16 column")?,
            })
        };
        Ok(match self.operation {
            LanguageToolOperation::Diagnostics => Query::Synchronize,
            LanguageToolOperation::Symbols => Query::Symbols,
            LanguageToolOperation::WorkspaceSymbols => Query::WorkspaceSymbols {
                query: self.query.clone().unwrap_or_default(),
            },
            LanguageToolOperation::Definitions => Query::Definition {
                position: position()?,
            },
            LanguageToolOperation::References => Query::References {
                position: position()?,
            },
            LanguageToolOperation::ResolveSymbol => Query::ResolveSymbol {
                id: self
                    .id
                    .clone()
                    .ok_or("Resolving a symbol requires its opaque ID")?,
            },
            LanguageToolOperation::ReadResource => Query::ReadResource {
                id: self
                    .id
                    .clone()
                    .ok_or("Reading a definition resource requires its opaque ID")?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_read_only_queries_and_operation_specific_arguments_are_accepted() {
        for value in [
            json!({"operation":"definitions","path":"a.rs"}),
            json!({"operation":"references","path":"a.rs","line":0,"column":1}),
            json!({"operation":"symbols","path":"../secret"}),
            json!({"operation":"symbols","path":"/absolute"}),
            json!({"operation":"symbols","path":"a.rs","line":1}),
            json!({"operation":"diagnostics","path":"a.rs","query":"hidden"}),
            json!({"operation":"workspace_symbols","path":"a.rs","query":"x\n"}),
            json!({"operation":"resolve_symbol","path":"a.rs"}),
            json!({"operation":"read_resource","path":"a.rs","id":"","offset":0}),
            json!({"operation":"read_resource","path":"a.rs","id":"a","limit":2001}),
        ] {
            assert!(
                serde_json::from_value::<LanguageToolArgs>(value.clone())
                    .unwrap()
                    .query()
                    .is_err(),
                "{value}"
            );
        }
        for value in [
            json!({"operation":"rename","path":"a.rs","new_name":"changed"}),
            json!({"operation":"execute_command","path":"a.rs"}),
            json!({"operation":"symbols","path":"a.rs","project":2}),
            json!({"operation":"diagnostics","path":"a.rs","source":"model supplied"}),
        ] {
            assert!(serde_json::from_value::<LanguageToolArgs>(value).is_err());
        }
        let args: LanguageToolArgs = serde_json::from_value(
            json!({"operation":"definitions","path":"a.rs","line":2,"column":4}),
        )
        .unwrap();
        assert!(matches!(
            args.query().unwrap(),
            super::super::intelligence::Query::Definition {
                position: super::super::intelligence::Position {
                    line: 1,
                    character: 3
                }
            }
        ));
    }
}
