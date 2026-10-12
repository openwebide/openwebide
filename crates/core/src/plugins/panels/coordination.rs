//! Durable instance coordination shared by both execution host transports.
use super::{PanelDocument, PanelInput};
use crate::plugins::PreparedPlugin;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginPanel {
    pub project: Option<i64>,
    pub prepared: PreparedPlugin,
    pub input: PanelInput,
    pub approved: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PanelClaim {
    Claimed { input: PanelInput },
    Pending,
    Completed { document: PanelDocument },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishPanel {
    pub instance: String,
    pub request: String,
    pub outcome: Result<PanelDocument, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosePanel {
    pub instance: String,
}
