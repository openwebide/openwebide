//! Panel validation and interaction policy shared by clients and execution hosts.
pub use openwebide_plugin_sdk::panels::*;
pub mod coordination;
pub mod registry;
pub fn view_request_allowed(capability: &str, payload: &str) -> bool {
    if capability == "http" {
        return serde_json::from_str::<openwebide_plugin_sdk::HttpRequest>(payload).is_ok_and(
            |request| {
                matches!(request.method.as_str(), "GET" | "HEAD") && request.body_base64.is_empty()
            },
        );
    }
    super::execution::context_request_allowed(capability, payload)
}
use std::collections::BTreeSet;

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}
fn text(value: &str, limit: usize) -> Result<(), String> {
    if value.len() > limit || value.contains('\0') {
        Err("Panel text exceeds its limit or contains NUL bytes".into())
    } else {
        Ok(())
    }
}
pub fn validate_definition(panel: &PanelDefinition) -> Result<(), String> {
    if !identifier(&panel.id)
        || !identifier(&panel.view)
        || panel.title.trim().is_empty()
        || panel.title.len() > 128
        || panel.actions.len() > 32
    {
        return Err("Panels require bounded titles, stable IDs and declared handlers".into());
    }
    if panel
        .refresh_seconds
        .is_some_and(|seconds| !(5..=3600).contains(&seconds))
    {
        return Err("Panel refresh intervals must be 5–3600 seconds".into());
    }
    let mut actions = BTreeSet::new();
    for action in &panel.actions {
        if !identifier(&action.id)
            || !identifier(&action.handler)
            || action.title.trim().is_empty()
            || action.title.len() > 128
            || !actions.insert(&action.id)
        {
            return Err("Panel actions need unique IDs, titles and declared handlers".into());
        }
    }
    Ok(())
}

pub fn validate_document(panel: &PanelDefinition, document: &PanelDocument) -> Result<(), String> {
    validate_definition(panel)?;
    if document.schema_version != UI_VERSION || document.revision == 0 {
        return Err("Unsupported panel UI schema or revision".into());
    }
    if serde_json::to_vec(document)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_DOCUMENT_BYTES
    {
        return Err("Panel document exceeds 256 KiB".into());
    }
    if serde_json::to_vec(&document.state)
        .map_err(|error| error.to_string())?
        .len()
        > 32 * 1024
    {
        return Err("Panel state exceeds its limit".into());
    }
    let mut count = 0;
    let mut fields = BTreeSet::new();
    visit(panel, &document.blocks, 0, &mut count, &mut fields)
}
fn button(panel: &PanelDefinition, button: &PanelButton) -> Result<(), String> {
    if button.label.trim().is_empty()
        || button.label.len() > 128
        || !panel
            .actions
            .iter()
            .any(|action| action.id == button.action)
    {
        return Err(
            "Panel buttons must reference a declared action and have a bounded label".into(),
        );
    }
    values(&button.values)
}
fn values(values: &std::collections::BTreeMap<String, serde_json::Value>) -> Result<(), String> {
    if values.len() > 32
        || values.keys().any(|key| !identifier(key))
        || serde_json::to_vec(values)
            .map_err(|error| error.to_string())?
            .len()
            > 32 * 1024
    {
        return Err("Panel inputs exceed their limit".into());
    }
    Ok(())
}
fn field(field: &PanelField, seen: &mut BTreeSet<String>) -> Result<(), String> {
    if !identifier(&field.id)
        || !seen.insert(field.id.clone())
        || field.label.trim().is_empty()
        || field.label.len() > 128
    {
        return Err("Panel fields require unique IDs and bounded labels".into());
    }
    match &field.kind {
        FieldKind::Text { max_length, .. } => {
            if !(1..=32768).contains(max_length)
                || !field.value.is_null()
                    && !field
                        .value
                        .as_str()
                        .is_some_and(|value| value.len() <= *max_length)
            {
                return Err("Invalid panel text field".into());
            }
        }
        FieldKind::Secret { max_length } => {
            if !(1..=32768).contains(max_length) || !field.value.is_null() {
                return Err(
                    "Secret fields require bounded length and an empty initial value".into(),
                );
            }
        }
        FieldKind::Checkbox => {
            if !field.value.is_null() && !field.value.is_boolean() {
                return Err("Invalid panel checkbox value".into());
            }
        }
        FieldKind::Select { options } => {
            let mut unique = BTreeSet::new();
            if options.is_empty()
                || options.len() > 100
                || options.iter().any(|option| {
                    option.value.len() > 256
                        || option.label.is_empty()
                        || option.label.len() > 256
                        || !unique.insert(&option.value)
                })
                || !field.value.is_null()
                    && !field
                        .value
                        .as_str()
                        .is_some_and(|value| options.iter().any(|option| option.value == value))
            {
                return Err("Invalid panel select options or value".into());
            }
        }
    }
    Ok(())
}
fn visit(
    panel: &PanelDefinition,
    blocks: &[Block],
    depth: usize,
    count: &mut usize,
    fields: &mut BTreeSet<String>,
) -> Result<(), String> {
    if depth > 6 {
        return Err("Panel layout nesting exceeds six levels".into());
    }
    for block in blocks {
        *count += 1;
        if *count > MAX_BLOCKS {
            return Err("Panel contains too many blocks".into());
        }
        match block {
            Block::Text { text: value }
            | Block::Heading { text: value }
            | Block::Status { text: value, .. } => text(value, 32768)?,
            Block::Group { title, blocks } => {
                if let Some(title) = title {
                    text(title, 128)?;
                }
                visit(panel, blocks, depth + 1, count, fields)?;
            }
            Block::Actions { buttons } => {
                if buttons.len() > 8 {
                    return Err("Too many panel action buttons".into());
                }
                for item in buttons {
                    button(panel, item)?;
                }
            }
            Block::Form {
                id,
                fields: inputs,
                submit,
            } => {
                if !identifier(id) || inputs.is_empty() || inputs.len() > 16 {
                    return Err("Invalid panel form".into());
                }
                for input in inputs {
                    field(input, fields)?;
                }
                if inputs
                    .iter()
                    .any(|input| submit.values.contains_key(&input.id))
                {
                    return Err("Panel form fields cannot replace button values".into());
                }
                button(panel, submit)?;
            }
            Block::List { items } => {
                if items.len() > 100 {
                    return Err("Panel list exceeds its limit".into());
                }
                for item in items {
                    text(item, 8192)?;
                }
            }
            Block::Table { columns, rows } => {
                if columns.is_empty()
                    || columns.len() > 8
                    || rows.len() > 100
                    || rows.iter().any(|row| row.len() != columns.len())
                {
                    return Err("Invalid panel table dimensions".into());
                }
                for value in columns.iter().chain(rows.iter().flatten()) {
                    text(value, 8192)?;
                }
            }
            Block::Code {
                text: value,
                language,
            } => {
                text(value, 65536)?;
                if let Some(language) = language {
                    text(language, 64)?;
                }
            }
            Block::Diff {
                before,
                after,
                path,
            } => {
                text(before, 65536)?;
                text(after, 65536)?;
                if let Some(path) = path {
                    text(path, 4096)?;
                }
            }
            Block::Link { text: label, url } => {
                text(label, 256)?;
                let link = url::Url::parse(url).map_err(|_| "Invalid panel link")?;
                if url.len() > 2048
                    || !matches!(link.scheme(), "https" | "http")
                    || link.host_str().is_none()
                    || !link.username().is_empty()
                    || link.password().is_some()
                {
                    return Err("Panel links require HTTP(S) URLs without credentials".into());
                }
            }
            Block::Divider => {}
        }
    }
    Ok(())
}

/// Approval previews preserve inspectable routing/body values while masking secrets.
pub fn approval_values(
    document: &PanelDocument,
    submitted: &std::collections::BTreeMap<String, serde_json::Value>,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    fn redact(
        blocks: &[Block],
        values: &mut std::collections::BTreeMap<String, serde_json::Value>,
    ) {
        for block in blocks {
            match block {
                Block::Group { blocks, .. } => redact(blocks, values),
                Block::Form { fields, .. } => {
                    for field in fields {
                        if matches!(field.kind, FieldKind::Secret { .. })
                            && values.contains_key(&field.id)
                        {
                            values.insert(
                                field.id.clone(),
                                serde_json::Value::String("<hidden>".into()),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut values = submitted.clone();
    redact(&document.blocks, &mut values);
    values
}

/// Validate an interaction against the authoritative document, rather than client state.
/// Callers must load this document from the owning instance's durable revision.
pub fn validate_submission(
    panel: &PanelDefinition,
    document: &PanelDocument,
    input: &PanelInput,
    approved: bool,
) -> Result<(), String> {
    validate_document(panel, document)?;
    validate_input(panel, input, approved)?;
    if input.revision != document.revision || input.state != document.state {
        return Err("Panel view or state changed; refresh before trying again".into());
    }
    if input.action.is_none() {
        return Ok(());
    }
    if !offered_action(&document.blocks, input) {
        return Err("Panel action is unavailable or its inputs are invalid".into());
    }
    Ok(())
}

fn offered_action(blocks: &[Block], input: &PanelInput) -> bool {
    blocks.iter().any(|block| match block {
        Block::Group { blocks, .. } => offered_action(blocks, input),
        Block::Actions { buttons } => buttons
            .iter()
            .any(|button| submitted_button(button, &[], input)),
        Block::Form { fields, submit, .. } => submitted_button(submit, fields, input),
        _ => false,
    })
}

fn submitted_button(button: &PanelButton, fields: &[PanelField], input: &PanelInput) -> bool {
    if button.disabled || input.action.as_deref() != Some(button.action.as_str()) {
        return false;
    }
    // Button values are immutable routing data. Form fields cannot replace them.
    if button
        .values
        .iter()
        .any(|(key, value)| input.values.get(key) != Some(value))
        || fields
            .iter()
            .any(|field| button.values.contains_key(&field.id))
        || input.values.keys().any(|key| {
            !button.values.contains_key(key) && !fields.iter().any(|field| field.id == *key)
        })
    {
        return false;
    }
    fields.iter().all(|field| {
        let Some(value) = input.values.get(&field.id).filter(|value| !value.is_null()) else {
            return !field.required;
        };
        match &field.kind {
            FieldKind::Text { max_length, .. } | FieldKind::Secret { max_length } => {
                value.as_str().is_some_and(|text| {
                    text.len() <= *max_length
                        && !text.contains('\0')
                        && (!field.required || !text.trim().is_empty())
                })
            }
            FieldKind::Checkbox => value
                .as_bool()
                .is_some_and(|checked| !field.required || checked),
            FieldKind::Select { options } => value
                .as_str()
                .is_some_and(|selected| options.iter().any(|option| option.value == selected)),
        }
    })
}

pub fn validate_input(
    panel: &PanelDefinition,
    input: &PanelInput,
    approved: bool,
) -> Result<(), String> {
    validate_definition(panel)?;
    if input.panel != panel.id
        || input.instance.len() != 32
        || input.request.len() != 32
        || !input
            .instance
            .bytes()
            .chain(input.request.bytes())
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Panel invocation identity is invalid".into());
    }
    values(&input.values)?;
    if serde_json::to_vec(&input.state)
        .map_err(|error| error.to_string())?
        .len()
        > 32 * 1024
    {
        return Err("Panel state exceeds its limit".into());
    }
    if let Some(action) = &input.action {
        let definition = panel
            .actions
            .iter()
            .find(|definition| definition.id == *action)
            .ok_or("Panel action is not declared")?;
        if definition.handler != input.handler || input.revision == 0 {
            return Err("Panel handler or view revision changed".into());
        }
        if definition.requires_approval && !approved {
            return Err("This panel action requires approval".into());
        }
    } else if input.handler != panel.view || !input.values.is_empty() {
        return Err("Invalid panel view request".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn panel() -> PanelDefinition {
        PanelDefinition {
            id: "pull-requests".into(),
            title: "Pull requests".into(),
            icon: PanelIcon::GitPullRequest,
            view: "render".into(),
            actions: vec![PanelAction {
                id: "publish".into(),
                handler: "publish".into(),
                title: "Publish review".into(),
                requires_approval: true,
            }],
            refresh_seconds: Some(30),
        }
    }
    #[test]
    fn documents_reject_unknown_actions_scripts_duplicate_fields_and_limits() {
        let mut doc = PanelDocument {
            schema_version: 1,
            revision: 1,
            blocks: vec![Block::Link {
                text: "PR".into(),
                url: "javascript:alert(1)".into(),
            }],
            state: serde_json::Value::Null,
        };
        assert!(validate_document(&panel(), &doc).is_err());
        doc.blocks = vec![Block::Actions {
            buttons: vec![PanelButton {
                action: "undeclared".into(),
                label: "Run".into(),
                values: Default::default(),
                disabled: false,
            }],
        }];
        assert!(validate_document(&panel(), &doc).is_err());
        doc.blocks = vec![Block::Text {
            text: "Safe plain text".into(),
        }];
        assert!(validate_document(&panel(), &doc).is_ok());
        let duplicate = PanelField {
            id: "body".into(),
            label: "Body".into(),
            kind: FieldKind::Text {
                multiline: true,
                max_length: 100,
            },
            value: serde_json::Value::Null,
            required: false,
        };
        doc.blocks = vec![Block::Form {
            id: "review".into(),
            fields: vec![duplicate.clone(), duplicate],
            submit: PanelButton {
                action: "publish".into(),
                label: "Publish".into(),
                values: Default::default(),
                disabled: false,
            },
        }];
        assert!(validate_document(&panel(), &doc).is_err());
        doc.blocks.clear();
        doc.schema_version = 99;
        assert!(validate_document(&panel(), &doc).is_err());
        doc.schema_version = 1;
        doc.blocks = vec![Block::Divider; 257];
        assert!(validate_document(&panel(), &doc).is_err());
        let raw = r#"{"type":"text","text":"x","html":"<script>"}"#;
        assert!(serde_json::from_str::<Block>(raw).is_err());
    }
    #[test]
    fn actions_bind_handler_scope_revision_and_approval() {
        let mut input = PanelInput {
            panel: "pull-requests".into(),
            instance: "a".repeat(32),
            request: "b".repeat(32),
            revision: 1,
            handler: "publish".into(),
            action: Some("publish".into()),
            values: Default::default(),
            state: serde_json::Value::Null,
        };
        assert!(validate_input(&panel(), &input, false).is_err());
        assert!(validate_input(&panel(), &input, true).is_ok());
        input.handler = "undeclared".into();
        assert!(validate_input(&panel(), &input, true).is_err());
        input.handler = "publish".into();
        input.revision = 0;
        assert!(validate_input(&panel(), &input, true).is_err());
    }
    #[test]
    fn submissions_reject_hidden_disabled_tampered_and_stale_actions() {
        let mut document = PanelDocument {
            schema_version: UI_VERSION,
            revision: 4,
            state: serde_json::json!({"repository": "owner/repo"}),
            blocks: vec![Block::Form {
                id: "review".into(),
                fields: vec![PanelField {
                    id: "body".into(),
                    label: "Review".into(),
                    kind: FieldKind::Text {
                        multiline: true,
                        max_length: 10,
                    },
                    value: serde_json::Value::Null,
                    required: true,
                }],
                submit: PanelButton {
                    action: "publish".into(),
                    label: "Publish".into(),
                    values: [("number".into(), serde_json::json!(42))].into(),
                    disabled: false,
                },
            }],
        };
        let mut input = PanelInput {
            panel: "pull-requests".into(),
            instance: "a".repeat(32),
            request: "b".repeat(32),
            revision: 4,
            handler: "publish".into(),
            action: Some("publish".into()),
            values: [
                ("number".into(), serde_json::json!(42)),
                ("body".into(), serde_json::json!("Looks good")),
            ]
            .into(),
            state: document.state.clone(),
        };
        assert!(validate_submission(&panel(), &document, &input, true).is_ok());
        assert!(validate_submission(&panel(), &document, &input, false).is_err());
        for value in [serde_json::json!(43), serde_json::Value::Null] {
            input.values.insert("number".into(), value);
            assert!(validate_submission(&panel(), &document, &input, true).is_err());
        }
        input.values.insert("number".into(), serde_json::json!(42));
        for value in [
            serde_json::json!(" "),
            serde_json::json!("01234567890"),
            serde_json::json!(false),
            serde_json::Value::Null,
        ] {
            input.values.insert("body".into(), value);
            assert!(validate_submission(&panel(), &document, &input, true).is_err());
        }
        input
            .values
            .insert("body".into(), serde_json::json!("Looks good"));
        input.values.insert("extra".into(), serde_json::json!(true));
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
        input.values.remove("extra");
        input.revision = 3;
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
        input.revision = 4;
        input.state = serde_json::Value::Null;
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
        input.state = document.state.clone();
        if let Block::Form { submit, .. } = &mut document.blocks[0] {
            submit.disabled = true;
        }
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
        document.blocks.clear();
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
    }
    #[test]
    fn required_checkboxes_and_selects_use_native_form_semantics() {
        let document: PanelDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"revision":1,"blocks":[{"type":"form","id":"review",
            "fields":[
                {"id":"agree","label":"Confirm","kind":{"type":"checkbox"},"required":true},
                {"id":"decision","label":"Decision","kind":{"type":"select","options":[{"value":"approve","label":"Approve"}]},"required":true}
            ],"submit":{"action":"publish","label":"Publish"}}]
        })).unwrap();
        let mut input = PanelInput {
            panel: "pull-requests".into(),
            instance: "a".repeat(32),
            request: "b".repeat(32),
            revision: 1,
            handler: "publish".into(),
            action: Some("publish".into()),
            values: [
                ("agree".into(), serde_json::json!(false)),
                ("decision".into(), serde_json::json!("approve")),
            ]
            .into(),
            state: serde_json::Value::Null,
        };
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
        input.values.insert("agree".into(), serde_json::json!(true));
        assert!(validate_submission(&panel(), &document, &input, true).is_ok());
        input
            .values
            .insert("decision".into(), serde_json::json!("undeclared"));
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
        input.values.remove("decision");
        assert!(validate_submission(&panel(), &document, &input, true).is_err());
    }
    #[test]
    fn secret_fields_cannot_reflect_saved_credentials_and_approval_masks_them() {
        let mut document: PanelDocument=serde_json::from_value(serde_json::json!({
            "schema_version":1,"revision":1,"blocks":[{"type":"form","id":"credentials","fields":[
                {"id":"token","label":"Token","kind":{"type":"secret","max_length":100},"required":true}
            ],"submit":{"action":"publish","label":"Save"}}]
        })).unwrap();
        assert!(validate_document(&panel(), &document).is_ok());
        let values = [
            ("token".into(), serde_json::json!("private-token")),
            ("number".into(), serde_json::json!(42)),
        ]
        .into();
        let preview = approval_values(&document, &values);
        assert_eq!(preview["token"], serde_json::json!("<hidden>"));
        assert_eq!(preview["number"], serde_json::json!(42));
        assert_eq!(values["token"], serde_json::json!("private-token"));
        if let Block::Form { fields, .. } = &mut document.blocks[0] {
            fields[0].value = serde_json::json!("private-token");
        }
        assert!(validate_document(&panel(), &document).is_err());
    }
}
