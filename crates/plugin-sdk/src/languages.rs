//! Versioned host language contributions. Clients render data and apply declared
//! editing rules; language implementations execute through the component host.
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const METADATA_HANDLER: &str = "@openwebide/languages";
pub const SERVICE_HANDLER: &str = "@openwebide/language";
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// Includes worst-case JSON escaping of a full admitted source and metadata.
pub const MAX_REQUEST_BYTES: usize = 6 * MAX_SOURCE_BYTES + 64 * 1024;
/// The invocation envelope contains a JSON-encoded language result.
pub const MAX_ENVELOPE_BYTES: usize = 2 * MAX_MESSAGE_BYTES + 1024 * 1024;
pub const MAX_RECORDS: usize = 100_000;

/// Parser-toolkit tags retained for existing syntax snapshots. Contribution
/// identities use strings and are selected through the language registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyntaxLanguage {
    Rust,
    Python,
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Java,
    CSharp,
    Php,
    Json,
    Html,
    Css,
    Markdown,
    MarkdownInline,
    Ini,
    Xml,
    Shell,
    Toml,
    Yaml,
    Sql,
    C,
    Cpp,
    Go,
    Plain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LanguageDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub filenames: Vec<String>,
    #[serde(default)]
    pub filename_patterns: Vec<String>,
    #[serde(default)]
    pub first_line: Option<String>,
    #[serde(default)]
    pub configuration: LanguageConfiguration,
    #[serde(default)]
    pub features: LanguageFeatures,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LanguageConfiguration {
    /// Line commenting uses this configuration throughout the document, even
    /// before analysis. Embedded scopes must preserve its line-comment strategy.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uniform_line_comments: bool,
    #[serde(default)]
    pub line_comment: Option<String>,
    #[serde(default)]
    pub block_comment: Option<CommentPair>,
    #[serde(default)]
    pub brackets: Vec<CharacterPair>,
    #[serde(default)]
    pub auto_closing_pairs: Vec<AutoClosingPair>,
    #[serde(default)]
    pub surrounding_pairs: Vec<CharacterPair>,
    #[serde(default)]
    pub indentation: IndentationRules,
    #[serde(default)]
    pub word_pattern: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentPair {
    pub open: String,
    pub close: String,
    #[serde(default)]
    pub nested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterPair {
    pub open: char,
    pub close: char,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AutoClosingPair {
    pub open: char,
    pub close: char,
    #[serde(default)]
    pub not_in: Vec<RegionKind>,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IndentationRules {
    #[serde(default)]
    pub increase: Option<String>,
    #[serde(default)]
    pub decrease: Option<String>,
    #[serde(default)]
    pub indent_next_line: Option<String>,
    #[serde(default)]
    pub on_enter: Vec<EnterRule>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnterRule {
    pub before: String,
    #[serde(default)]
    pub after: Option<String>,
    pub action: EnterAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnterAction {
    Indent,
    Outdent,
    IndentOutdent,
    Keep,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LanguageFeatures {
    #[serde(default)]
    pub syntax: bool,
    #[serde(default)]
    pub folding: bool,
    #[serde(default)]
    pub structure: bool,
    #[serde(default)]
    pub snippets: Vec<Snippet>,
    #[serde(default)]
    pub formatting: Option<LanguageHandler>,
    #[serde(default)]
    pub diagnostics: Vec<LanguageHandler>,
    #[serde(default)]
    pub language_server: Option<LanguageServer>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    pub name: String,
    pub prefix: String,
    pub body: String,
}

/// A declared extension point; the host reports unsupported methods instead of
/// substituting another provider or silently changing save behavior.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageHandler {
    pub id: String,
    pub title: String,
}

/// Configuration for later host LSP support. Declaring a server does not launch
/// a process, install a runtime or grant process execution authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LanguageServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub initialization_options: serde_json::Value,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LanguageInput {
    Analyze(AnalysisRequest),
    Close { version: u32, document: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnalysisRequest {
    pub version: u32,
    pub document: String,
    pub request: u64,
    pub revision: u64,
    pub language: String,
    pub path: String,
    pub source_digest: String,
    pub source: SourceChange,
    pub tab_width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SourceChange {
    Full {
        text: String,
    },
    Replace {
        base_revision: u64,
        start: usize,
        end: usize,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LanguageOutput {
    Analyzed(Box<AnalysisResponse>),
    Closed { version: u32, document: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnalysisResponse {
    pub version: u32,
    pub document: String,
    pub request: u64,
    pub revision: u64,
    pub language: String,
    pub source_digest: String,
    pub status: AnalysisStatus,
    pub analysis: Option<SyntaxAnalysis>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnalysisStatus {
    Ready { incremental: bool },
    NeedsSource,
    TooLarge,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxAnalysis {
    /// UTF-8 end offsets relative to each source row. Source text is never
    /// supplied by the provider; clients paint their own validated snapshot.
    pub highlights: Option<Vec<Vec<TokenSpan>>>,
    pub folds: Vec<FoldRange>,
    pub structure: Option<Structure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenSpan {
    pub end: usize,
    pub kind: TokenKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenKind {
    Plain,
    Keyword,
    Type,
    String,
    Char,
    Comment,
    Number,
    Function,
    Operator,
    Punct,
    Attribute,
    Macro,
    Lifetime,
    Boolean,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A source-owned embedded body. The shared document service resolves its
/// language ID or alias through installed providers and user/project preferences.
/// Missing, disabled, ambiguous or failed providers leave the body plain.
pub struct LanguageScope {
    pub range: TextRange,
    pub language: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    String,
    Template,
    Text,
    LineComment,
    BlockComment,
    Regex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedRegion {
    pub range: TextRange,
    pub complete: bool,
    pub kind: RegionKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bracket {
    pub offset: usize,
    pub character: char,
    pub partner: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Structure {
    pub language: String,
    pub scopes: Vec<LanguageScope>,
    pub selections: Vec<TextRange>,
    pub opaque_starts: Vec<usize>,
    pub protected: Vec<ProtectedRegion>,
    pub brackets: Vec<Bracket>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uniform_line_comments_are_explicit_and_old_configuration_wire_stays_unchanged() {
        let mut configuration: LanguageConfiguration = serde_json::from_value(json!({
            "lineComment": "#"
        }))
        .unwrap();
        assert!(!configuration.uniform_line_comments);
        assert!(
            serde_json::to_value(&configuration)
                .unwrap()
                .get("uniformLineComments")
                .is_none()
        );
        configuration.uniform_line_comments = true;
        let wire = serde_json::to_value(&configuration).unwrap();
        assert_eq!(wire["uniformLineComments"], true);
        assert_eq!(
            serde_json::from_value::<LanguageConfiguration>(wire).unwrap(),
            configuration
        );
    }

    #[test]
    fn language_service_wire_round_trips_full_and_incremental_unicode_sources() {
        for source in [
            json!({"type":"full","text":"fn main() { /* 文😀 */ }\r\n"}),
            json!({"type":"replace","baseRevision":7,"start":0,"end":0,"text":"// 文😀\r\n"}),
        ] {
            let wire = json!({"operation":"analyze","version":VERSION,"document":"client/project/file","request":19,"revision":8,"language":"custom-language","path":"file.custom","sourceDigest":"a".repeat(64),"source":source,"tabWidth":4});
            let value: LanguageInput = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(value).unwrap(), wire);
        }
        assert!(
            serde_json::from_value::<LanguageInput>(
                json!({"operation":"analyze","version":1,"script":"run on client"})
            )
            .is_err()
        );
        let close = json!({"operation":"close","version":VERSION,"document":"client/project/file"});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<LanguageInput>(close.clone()).unwrap())
                .unwrap(),
            close
        );
    }
}
