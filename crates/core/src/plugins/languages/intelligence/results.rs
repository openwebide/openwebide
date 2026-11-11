//! Data consumed by every code-intelligence client. Coordinates and edits are
//! validated against exact retained source before entering editor history.
use super::{Range, relative_path};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: Option<String>,
    pub resource: Option<String>,
    pub source_digest: Option<String>,
    pub label: String,
    pub range: Range,
}
impl Target {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.is_some() == self.resource.is_some()
            || (self.path.is_some()
                && self.source_digest.as_ref().is_none_or(|digest| {
                    digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                }))
            || (self.resource.is_some() && self.source_digest.is_some())
            || self.path.as_ref().is_some_and(|path| !relative_path(path))
            || self.resource.as_ref().is_some_and(|id| {
                id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || self.label.is_empty()
            || self.label.len() > 4096
            || self.label.contains('\0')
            || self.range.start > self.range.end
        {
            return Err("Invalid language-server navigation target".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    pub name: String,
    pub kind: u32,
    pub target: Target,
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub kind: u32,
    pub container: Option<String>,
    pub target: Option<Target>,
    /// Opaque, source-bound identity for a lazily resolved location.
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEdit {
    pub path: String,
    pub source_digest: String,
    pub revision: Option<u64>,
    pub edits: Vec<TextEdit>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEdit {
    #[serde(default)]
    pub files: Vec<FileEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<WorkspaceChange>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceChange {
    Text {
        file: FileEdit,
    },
    File {
        operation: crate::workspace_transaction::FileOperation,
    },
}
impl WorkspaceEdit {
    pub fn validate(&self) -> Result<(), String> {
        for change in self.clone().ordered()? {
            match change {
                WorkspaceChange::File { operation } => operation.validate()?,
                WorkspaceChange::Text { file } => {
                    if !super::relative_path(&file.path)
                        || file.source_digest.len() != 64
                        || !file
                            .source_digest
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                        || file.edits.len() > super::MAX_RECORDS
                        || file
                            .edits
                            .iter()
                            .any(|edit| edit.range.start > edit.range.end)
                    {
                        return Err("Invalid file changes in language action review".into());
                    }
                }
            }
        }
        Ok(())
    }
    pub fn ordered(self) -> Result<Vec<WorkspaceChange>, String> {
        if !self.files.is_empty() && !self.changes.is_empty() {
            return Err("Workspace edits cannot mix unordered and ordered changes".into());
        }
        let changes = if self.changes.is_empty() {
            self.files
                .into_iter()
                .map(|file| WorkspaceChange::Text { file })
                .collect()
        } else {
            self.changes
        };
        if changes.len() > super::MAX_RECORDS {
            return Err("Workspace edit has too many changes".into());
        }
        Ok(changes)
    }
}

/// One deterministic, nonoverlapping transaction. Preserve the server's order
/// for multiple insertions at the same boundary; no partial edit is returned.
pub fn editor_edits(source: &str, edits: &[TextEdit]) -> Result<Vec<crate::editor::Edit>, String> {
    if edits.len() > crate::plugins::languages::MAX_RECORDS {
        return Err("Too many language-server text edits".into());
    }
    let mut result = edits
        .iter()
        .map(|edit| {
            Ok(crate::editor::Edit::replace(
                edit.range.offsets(source)?,
                edit.new_text.clone(),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    result.sort_by_key(|edit| (edit.range.start, edit.range.end));
    let mut end = 0;
    let mut size = source.len();
    for edit in &result {
        if edit.range.start < end {
            return Err("Language-server edits overlap".into());
        }
        size = size
            .checked_sub(edit.range.len())
            .and_then(|size| size.checked_add(edit.text.len()))
            .ok_or("Language-server edit size overflow")?;
        end = edit.range.end;
    }
    if size > crate::editor::MAX_DOCUMENT_BYTES {
        return Err("Language-server edits exceed the document limit".into());
    }
    Ok(result)
}

pub fn apply_edits(source: &str, edits: &[TextEdit]) -> Result<String, String> {
    let edits = editor_edits(source, edits)?;
    let mut result = String::new();
    let mut start = 0;
    for edit in edits {
        result.push_str(&source[start..edit.range.start]);
        result.push_str(&edit.text);
        start = edit.range.end;
    }
    result.push_str(&source[start..]);
    Ok(result)
}

/// Right-affine source caret, accounting for imports preceding a completion.
pub fn caret_after(source: &str, edits: &[TextEdit], caret: usize) -> Result<usize, String> {
    super::position(source, caret)?;
    let edits = editor_edits(source, edits)?;
    let mut before = 0;
    let mut after = 0;
    for edit in edits {
        if caret < edit.range.start {
            break;
        }
        after += edit.range.start - before;
        if caret <= edit.range.end {
            return Ok(after + edit.text.len());
        }
        after += edit.text.len();
        before = edit.range.end;
    }
    Ok(after + caret - before)
}

/// Completion insertion is a single editor transaction, including imports.
pub fn completion_edits(
    source: &str,
    position: super::Position,
    item: &serde_json::Value,
) -> Result<Vec<TextEdit>, String> {
    completion(source, position, item, &std::collections::BTreeMap::new())
        .map(|completion| completion.edits)
}

pub struct Completion {
    pub edits: Vec<TextEdit>,
    pub snippet: Option<crate::editor::snippet::Snippet>,
    pub suggest: bool,
}

pub fn completion(
    source: &str,
    position: super::Position,
    item: &serde_json::Value,
    variables: &std::collections::BTreeMap<String, String>,
) -> Result<Completion, String> {
    let command = &item["command"];
    let suggest = command["command"].as_str() == Some("editor.action.triggerSuggest")
        && (command["arguments"].is_null()
            || command["arguments"].as_array().is_some_and(Vec::is_empty));
    if item["insertTextFormat"]
        .as_u64()
        .is_some_and(|format| !matches!(format, 1 | 2))
        || (!command.is_null() && !suggest)
    {
        return Err("This completion requires unsupported insertion or command behavior".into());
    }
    let mut edit: TextEdit = if let Some(edit) = item.get("textEdit").filter(|edit| !edit.is_null())
    {
        if edit.get("range").is_some() {
            serde_json::from_value(edit.clone()).map_err(|_| "Invalid completion edit")?
        } else {
            TextEdit {
                range: serde_json::from_value(edit["replace"].clone())
                    .map_err(|_| "Invalid completion replacement")?,
                new_text: edit["newText"]
                    .as_str()
                    .ok_or("Missing completion text")?
                    .into(),
            }
        }
    } else {
        let end = super::offset(source, position)?;
        let start = source[..end]
            .char_indices()
            .rev()
            .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
            .last()
            .map_or(end, |(index, _)| index);
        TextEdit {
            range: super::Range {
                start: super::position(source, start)?,
                end: position,
            },
            new_text: item["insertText"]
                .as_str()
                .or_else(|| item["label"].as_str())
                .ok_or("Missing completion text")?
                .into(),
        }
    };
    let mut snippet = if item["insertTextFormat"].as_u64() == Some(2) {
        let snippet = crate::editor::snippet::Snippet::expand(&edit.new_text, variables)?;
        edit.new_text.clone_from(&snippet.text);
        Some(snippet)
    } else {
        None
    };
    let mut edits = vec![edit];
    if let Some(additional) = item
        .get("additionalTextEdits")
        .filter(|value| !value.is_null())
    {
        edits.extend(
            serde_json::from_value::<Vec<TextEdit>>(additional.clone())
                .map_err(|_| "Invalid completion import edits")?,
        );
    }
    let validated = editor_edits(source, &edits)?;
    if let Some(snippet) = &mut snippet {
        let primary = edits[0].range.offsets(source)?;
        let mut order = edits
            .iter()
            .enumerate()
            .map(|(index, edit)| Ok((index, edit.range.offsets(source)?)))
            .collect::<Result<Vec<_>, String>>()?;
        order.sort_by_key(|(_, range)| (range.start, range.end));
        let mut start = primary.start;
        for ((index, _), edit) in order.iter().zip(&validated) {
            if *index == 0 {
                break;
            }
            start = start
                .checked_sub(edit.range.len())
                .and_then(|start| start.checked_add(edit.text.len()))
                .ok_or("Completion placeholder offset overflow")?;
        }
        for range in snippet.fields.values_mut().flatten().chain(
            snippet
                .transforms
                .iter_mut()
                .map(|transform| &mut transform.range),
        ) {
            range.start += start;
            range.end += start;
        }
    }
    Ok(Completion {
        edits,
        snippet,
        suggest,
    })
}

pub fn hover(result: &serde_json::Value) -> Result<Option<String>, String> {
    if result.is_null() {
        return Ok(None);
    }
    fn content(value: &serde_json::Value) -> Option<String> {
        if let Some(text) = value.as_str() {
            return Some(text.into());
        }
        let text = value.get("value")?.as_str()?;
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("plaintext") {
            return Some(
                text.chars()
                    .flat_map(|ch| {
                        if "\\`*_{}[]<>#!|".contains(ch) {
                            vec!['\\', ch]
                        } else {
                            vec![ch]
                        }
                    })
                    .collect(),
            );
        }
        if let Some(language) = value.get("language").and_then(serde_json::Value::as_str) {
            let language = language
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-'))
                .take(64)
                .collect::<String>();
            return Some(format!("````{language}\n{text}\n````"));
        }
        Some(text.into())
    }
    let contents = &result["contents"];
    let text = if let Some(values) = contents.as_array() {
        values
            .iter()
            .map(content)
            .collect::<Option<Vec<_>>>()
            .map(|values| values.join("\n\n"))
    } else {
        content(contents)
    }
    .ok_or("Invalid language-server hover content")?;
    if text.len() > super::MAX_RESULT_BYTES {
        return Err("Language-server hover exceeds its byte limit".into());
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::languages::intelligence::Position;
    fn edit(start: u32, end: u32, text: &str) -> TextEdit {
        TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: start,
                },
                end: Position {
                    line: 0,
                    character: end,
                },
            },
            new_text: text.into(),
        }
    }
    #[test]
    fn edits_preserve_unicode_crlf_order_and_reject_surrogates_and_overlaps() {
        let source = "a😀z\r\n";
        assert_eq!(
            apply_edits(source, &[edit(1, 3, "水"), edit(4, 4, "!")]).unwrap(),
            "a水z!\r\n"
        );
        assert_eq!(
            apply_edits("ab", &[edit(1, 1, "1"), edit(1, 1, "2")]).unwrap(),
            "a12b"
        );
        for edits in [
            vec![edit(1, 2, "x")],
            vec![edit(1, 3, "x"), edit(2, 3, "y")],
            vec![edit(4, 9, "x")],
        ] {
            assert!(apply_edits(source, &edits).is_err());
        }
    }
    #[test]
    fn completion_keeps_imports_in_one_transaction_and_maps_the_caret_to_the_inserted_name() {
        let source = "fn f() { Mat }";
        let item = serde_json::json!({"label":"Match","textEdit":{
            "range":{"start":{"line":0,"character":9},"end":{"line":0,"character":12}},
            "newText":"Match"},"additionalTextEdits":[{
                "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},
                "newText":"use example::Match;\n"}]});
        let edits = completion_edits(
            source,
            Position {
                line: 0,
                character: 12,
            },
            &item,
        )
        .unwrap();
        let after = apply_edits(source, &edits).unwrap();
        assert_eq!(after, "use example::Match;\nfn f() { Match }");
        assert_eq!(
            &after[..caret_after(source, &edits, 12).unwrap()],
            "use example::Match;\nfn f() { Match"
        );
        let mut invalid = item;
        invalid["insertTextFormat"] = serde_json::json!(3);
        assert!(
            completion_edits(
                source,
                Position {
                    line: 0,
                    character: 12
                },
                &invalid
            )
            .is_err()
        );
        invalid["insertTextFormat"] = serde_json::json!(1);
        invalid["additionalTextEdits"][0]["range"] = invalid["textEdit"]["range"].clone();
        assert!(
            completion_edits(
                source,
                Position {
                    line: 0,
                    character: 12
                },
                &invalid
            )
            .is_err()
        );
    }
    #[test]
    fn snippet_completion_imports_keep_linked_fields_and_one_insertion_undo_step() {
        use crate::editor::{Document, Selection};
        let source = "fn f() { ca }";
        let item = serde_json::json!({"label":"call", "insertTextFormat":2,
            "textEdit":{"range":{"start":{"line":0,"character":9},"end":{"line":0,"character":11}},"newText":"call(${1:文}, $1)$0"},
            "additionalTextEdits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"use call;\n"}]});
        let completion = completion(
            source,
            Position {
                line: 0,
                character: 11,
            },
            &item,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let mut document = Document::new(source);
        let edits = editor_edits(source, &completion.edits).unwrap();
        document
            .apply(edits, vec![Selection::caret(0)], None)
            .unwrap();
        document.start_snippet(completion.snippet.unwrap()).unwrap();
        assert_eq!(document.text(), "use call;\nfn f() { call(文, 文) }");
        for selected in document.selections() {
            assert_eq!(&document.text()[selected.range()], "文");
        }
        document.replace_selections("😀x", None).unwrap();
        assert_eq!(document.text(), "use call;\nfn f() { call(😀x, 😀x) }");
        document.snippet_next(false).unwrap();
        assert_eq!(
            &document.text()[..document.selections()[0].head],
            "use call;\nfn f() { call(😀x, 😀x)"
        );
        document.undo();
        document.undo();
        assert_eq!(document.text(), source);
    }
    #[test]
    fn hover_accepts_standard_lsp_content_forms_without_turning_plaintext_into_markup() {
        assert!(
            hover(&serde_json::json!({"contents":{"kind":"markdown","value":"**type**"}}))
                .unwrap()
                .unwrap()
                .contains("**type**")
        );
        assert_eq!(
            hover(&serde_json::json!({"contents":{"kind":"plaintext","value":"<T>"}})).unwrap(),
            Some("\\<T\\>".into())
        );
        assert!(
            hover(&serde_json::json!({"contents":["docs",{"language":"rust","value":"fn f()"}]}))
                .unwrap()
                .unwrap()
                .contains("fn f()")
        );
        assert!(hover(&serde_json::json!({"contents":42})).is_err());
    }
}
