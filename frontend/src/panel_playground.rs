//! Standalone authoring policy. It uses production panel validation and never host APIs.
use openwebide_core::plugins::panels::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// Pretty-printed authoring JSON includes indentation and a panel definition;
// the production document itself retains its compact 256 KiB limit.
pub const MAX_SOURCE_BYTES: usize = 2 * MAX_DOCUMENT_BYTES;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Playground {
    pub panel: PanelDefinition,
    pub document: PanelDocument,
}
impl Playground {
    pub fn blank() -> Self {
        Self {
            panel: PanelDefinition {
                id: "panel".into(),
                title: "New panel".into(),
                icon: PanelIcon::Puzzle,
                view: "render".into(),
                actions: vec![],
                refresh_seconds: None,
            },
            document: PanelDocument {
                schema_version: UI_VERSION,
                revision: 1,
                blocks: vec![],
                state: Value::Null,
            },
        }
    }
    pub fn parse(source: &str) -> Result<Self, String> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err("The panel definition and document exceed the editor limit".into());
        }
        let value: Self = serde_json::from_str(source).map_err(|error| error.to_string())?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), String> {
        validate_definition(&self.panel)?;
        validate_document(&self.panel, &self.document)
    }
    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serializable panel example")
    }
    /// A simulated replacement goes through the same authoritative submission checks.
    pub fn simulate(
        &self,
        input: &PanelInput,
        response: &str,
        approved: bool,
    ) -> Result<Self, String> {
        validate_submission(&self.panel, &self.document, input, approved)?;
        if response.len() > MAX_DOCUMENT_BYTES {
            return Err("Simulated response exceeds the document limit".into());
        }
        let document: PanelDocument =
            serde_json::from_str(response).map_err(|error| error.to_string())?;
        validate_document(&self.panel, &document)?;
        if input.revision.checked_add(1) != Some(document.revision) {
            return Err("Simulated response must return the next view revision".into());
        }
        Ok(Self {
            panel: self.panel.clone(),
            document,
        })
    }
    pub fn input(
        &self,
        action: String,
        values: std::collections::BTreeMap<String, Value>,
    ) -> Result<PanelInput, String> {
        let declaration = self
            .panel
            .actions
            .iter()
            .find(|candidate| candidate.id == action)
            .ok_or("Action is not declared")?;
        let input = PanelInput {
            panel: self.panel.id.clone(),
            instance: "1".repeat(32),
            request: "2".repeat(32),
            revision: self.document.revision,
            handler: declaration.handler.clone(),
            action: Some(action),
            values,
            state: self.document.state.clone(),
        };
        validate_submission(&self.panel, &self.document, &input, true)?;
        Ok(input)
    }
    pub fn response(&self) -> String {
        let mut document = self.document.clone();
        document.revision = document.revision.saturating_add(1);
        let pretty = serde_json::to_string_pretty(&document).expect("serializable response");
        if pretty.len() <= MAX_DOCUMENT_BYTES {
            pretty
        } else {
            serde_json::to_string(&document).expect("serializable response")
        }
    }
    pub fn rust(&self) -> Result<String, String> {
        self.validate()?;
        let panel =
            rust_literal(&serde_json::to_string(&self.panel).map_err(|error| error.to_string())?);
        let document = rust_literal(
            &serde_json::to_string(&self.document).map_err(|error| error.to_string())?,
        );
        Ok(format!(
            r#"use openwebide_plugin_sdk::{{Plugin, panels::*, serde_json}};

struct Example;
impl Plugin for Example {{
    fn panels() -> Vec<PanelDefinition> {{
        vec![serde_json::from_str({panel}).expect("valid panel definition")]
    }}
    fn panel(input: PanelInput) -> Result<PanelDocument, String> {{
        let mut document: PanelDocument = serde_json::from_str({document})
            .map_err(|error| error.to_string())?;
        if let Some(action) = &input.action {{
            // Implement declared actions here using the public SDK.
            // The host enforces capability grants, approvals and view ownership.
            let _ = action; // Replace this with your action behavior.
            // Keep replacement documents within the schema and resource limits.
        }}
        document.revision = input.revision.checked_add(1)
            .ok_or("View revision exhausted")?;
        Ok(document)
    }}
}}
openwebide_plugin_sdk::export!(Example);
"#
        ))
    }
}
fn rust_literal(text: &str) -> String {
    let mut hashes = String::from("#");
    while text.contains(&format!("\"{hashes}")) {
        hashes.push('#');
    }
    format!("r{hashes}\"{text}\"{hashes}")
}

pub fn gallery() -> Vec<(&'static str, Playground)> {
    let panel:PanelDefinition=serde_json::from_value(json!({"id":"example","title":"Example panel","icon":"puzzle","view":"render","actions":[{"id":"save","handler":"save","title":"Save settings","requires_approval":true},{"id":"refresh","handler":"refresh","title":"Refresh","requires_approval":false}]})).expect("gallery definition");
    let blocks: Vec<(&str, Value)> = vec![
        (
            "Text",
            json!({"type":"text","text":"Plugin-owned text uses the app’s typography and theme."}),
        ),
        ("Heading", json!({"type":"heading","text":"Pull requests"})),
        (
            "Status",
            json!({"type":"group","title":"Status notices","blocks":[{"type":"status","tone":"neutral","text":"Waiting for a review"},{"type":"status","tone":"success","text":"Review published"},{"type":"status","tone":"warning","text":"The commit changed"},{"type":"status","tone":"error","text":"Host unavailable"}]}),
        ),
        (
            "Group",
            json!({"type":"group","title":"Repository","blocks":[{"type":"text","text":"Group related controls and content."},{"type":"group","blocks":[{"type":"text","text":"Groups can nest within the document depth limit."}]}]}),
        ),
        (
            "Actions",
            json!({"type":"actions","buttons":[{"action":"refresh","label":"Refresh","values":{}},{"action":"save","label":"Save","values":{"destination":"demo"}},{"action":"refresh","label":"Unavailable","disabled":true,"values":{}}]}),
        ),
        (
            "Form",
            json!({"type":"form","id":"settings","fields":[{"id":"repository","label":"Repository","kind":{"type":"text","multiline":false,"max_length":128},"value":"openwebide/plugins","required":true},{"id":"message","label":"Review message","kind":{"type":"text","multiline":true,"max_length":1024},"value":"Looks good","required":true},{"id":"token","label":"Token","kind":{"type":"secret","max_length":256},"value":null},{"id":"notify","label":"Notify me","kind":{"type":"checkbox"},"value":true},{"id":"event","label":"Review type","kind":{"type":"select","options":[{"value":"COMMENT","label":"Comment"},{"value":"APPROVE","label":"Approve"}]},"value":"COMMENT","required":true}],"submit":{"action":"save","label":"Save settings","values":{"destination":"demo"}}}),
        ),
        (
            "List",
            json!({"type":"list","items":["Inspect the current commit","Read the diff","Approve publishing"]}),
        ),
        (
            "Table",
            json!({"type":"table","columns":["Pull request","Author","Status"],"rows":[["#42 Add panels","contributor","Open"],["#43 Add examples","author","Draft"]]}),
        ),
        (
            "Code",
            json!({"type":"code","language":"rust","text":"fn main() {\n    println!(\"Hello from a plugin\");\n}"}),
        ),
        (
            "Diff",
            json!({"type":"diff","path":"src/lib.rs","before":"fn title() {\n    old_title();\n}\n","after":"fn title() {\n    new_title();\n}\n"}),
        ),
        (
            "Link",
            json!({"type":"link","text":"Read the panel authoring guide","url":"https://openwebide.com/docs/plugin-panels.html"}),
        ),
        ("Divider", json!({"type":"divider"})),
    ];
    let mut examples = Vec::new();
    let all = blocks
        .iter()
        .map(|(_, block)| serde_json::from_value(block.clone()).expect("gallery block"))
        .collect();
    examples.push((
        "All components",
        Playground {
            panel: panel.clone(),
            document: PanelDocument {
                schema_version: UI_VERSION,
                revision: 1,
                state: json!({"example":true}),
                blocks: all,
            },
        },
    ));
    for (label, block) in blocks {
        examples.push((
            label,
            Playground {
                panel: panel.clone(),
                document: PanelDocument {
                    schema_version: UI_VERSION,
                    revision: 1,
                    state: Value::Null,
                    blocks: vec![serde_json::from_value(block).expect("gallery block")],
                },
            },
        ));
    }
    examples
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_gallery_example_passes_production_validation_and_exports() {
        assert_eq!(gallery().len(), 13);
        for (_, example) in gallery() {
            example.validate().unwrap();
            assert_eq!(Playground::parse(&example.json()).unwrap(), example);
            assert!(example.rust().unwrap().contains("export!(Example)"));
        }
    }
    #[test]
    fn simulations_require_offered_actions_approval_and_next_revision() {
        let example = &gallery()[5].1;
        let input = example
            .input(
                "save".into(),
                [("destination".into(), json!("demo"))].into(),
            )
            .unwrap();
        assert!(
            example
                .simulate(&input, &example.response(), false)
                .is_err()
        );
        assert_eq!(
            example
                .simulate(&input, &example.response(), true)
                .unwrap()
                .document
                .revision,
            2
        );
        let stale = serde_json::to_string(&example.document).unwrap();
        assert!(example.simulate(&input, &stale, true).is_err());
        assert!(
            example
                .input(
                    "refresh".into(),
                    [("destination".into(), json!("demo"))].into()
                )
                .is_err()
        );
    }
    #[test]
    fn exported_literals_cannot_escape_into_rust_code() {
        let value = "quoted \"### and braces {}";
        let literal = rust_literal(value);
        assert!(literal.starts_with("r####\""));
        assert!(literal.ends_with("\"####"));
    }
}
