//! Versioned declarative panels. Data only: no browser code, markup or expressions.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const UI_VERSION: u32 = 1;
pub const METADATA_HANDLER: &str = "@openwebide/panels";
pub const VIEW_HANDLER: &str = "@openwebide/panel";
pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
pub const MAX_BLOCKS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelDefinition {
    pub id: String,
    pub title: String,
    pub icon: PanelIcon,
    pub view: String,
    #[serde(default)]
    pub actions: Vec<PanelAction>,
    #[serde(default)]
    pub refresh_seconds: Option<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelIcon {
    Puzzle,
    GitPullRequest,
    List,
    Calendar,
    Activity,
    BookOpen,
    Terminal,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelAction {
    pub id: String,
    pub handler: String,
    pub title: String,
    pub requires_approval: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelInput {
    pub panel: String,
    pub instance: String,
    pub request: String,
    pub revision: u64,
    pub handler: String,
    pub action: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub state: serde_json::Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelDocument {
    pub schema_version: u32,
    pub revision: u64,
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub state: serde_json::Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Error,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Text {
        text: String,
    },
    Heading {
        text: String,
    },
    Status {
        text: String,
        tone: Tone,
    },
    Group {
        title: Option<String>,
        blocks: Vec<Block>,
    },
    Actions {
        buttons: Vec<PanelButton>,
    },
    Form {
        id: String,
        fields: Vec<PanelField>,
        submit: PanelButton,
    },
    List {
        items: Vec<String>,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Code {
        text: String,
        language: Option<String>,
    },
    Diff {
        before: String,
        after: String,
        path: Option<String>,
    },
    Link {
        text: String,
        url: String,
    },
    Divider,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelButton {
    pub action: String,
    pub label: String,
    #[serde(default)]
    pub values: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub disabled: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelField {
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default)]
    pub required: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldKind {
    Text { multiline: bool, max_length: usize },
    Secret { max_length: usize },
    Checkbox,
    Select { options: Vec<FieldOption> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}
