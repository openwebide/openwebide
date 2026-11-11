//! Public host code-intelligence protocol. Installed providers declare servers;
//! clients exchange source-bound data and never choose executable commands.
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 3;
pub const MAX_ANALYSIS_FILES: usize = 16_384;
pub const MAX_ANALYSIS_PATH_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ANALYSIS_IDENTITY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ANALYSIS_METADATA_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ANALYSIS_METADATA_RECORD_BYTES: usize = 32 * 1024;
/// API 5 server declarations remain metadata. API 6 opts in to native execution
/// of the exact installed declaration, without requiring a component wrapper.
pub const MIN_PLUGIN_API: u32 = 6;
pub const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DIAGNOSTICS: usize = 4096;
pub const MAX_RECORDS: usize = super::MAX_RECORDS;
pub mod actions;
pub mod results;
pub mod workspace_edit;

/// Native proof of the source/configuration context that produced a result.
/// Clients retain it unchanged and validate it on the original service before
/// using a reviewed result. It is not permission to mutate workspace files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisContext {
    /// Canonically ordered native observation paths. The opaque proof binds
    /// their original metadata as well as the result. Empty for overlay-only
    /// results and archived protocol-2 evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Opaque native object identities aligned with files. An empty entry
    /// represents absence or a non-resource link. Archived evidence omits the
    /// vector; it is never filesystem mutation authority by itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
    /// Opaque original native metadata aligned with files. Only the owning
    /// native adapter interprets these; archived evidence omits them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revisions: Vec<String>,
    pub server: String,
    pub result_digest: String,
    pub proof: String,
}
impl AnalysisContext {
    pub fn validate(&self) -> Result<(), String> {
        if (!self.revisions.is_empty() && self.revisions.len() != self.files.len())
            || self.revisions.iter().map(String::len).sum::<usize>() > MAX_ANALYSIS_METADATA_BYTES
            || self.revisions.iter().any(|revision| {
                revision.is_empty()
                    || revision.len() > MAX_ANALYSIS_METADATA_RECORD_BYTES
                    || revision.chars().any(char::is_control)
            })
            || (!self.identities.is_empty() && self.identities.len() != self.files.len())
            || self.identities.iter().map(String::len).sum::<usize>() > MAX_ANALYSIS_IDENTITY_BYTES
            || self.identities.iter().any(|identity| {
                !identity.is_empty()
                    && (identity.len() != 67
                        || !matches!(identity.get(..3), Some("u0." | "u1." | "w1."))
                        || !identity.as_bytes()[3..].iter().all(u8::is_ascii_hexdigit))
            })
            || self.files.len() > MAX_ANALYSIS_FILES
            || self.files.iter().map(String::len).sum::<usize>() > MAX_ANALYSIS_PATH_BYTES
            || self
                .files
                .iter()
                .any(|path| !path.is_empty() && !relative_path(path))
            || self.files.windows(2).any(|pair| pair[0] >= pair[1])
            || !super::text(&self.server, 256)
            || self.result_digest.len() != 64
            || !self
                .result_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.proof.is_empty()
            || self.proof.len() > 2048
            || !self
                .proof
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("Invalid language analysis context".into());
        }
        Ok(())
    }
    pub fn validate_result(&self, result: &serde_json::Value) -> Result<(), String> {
        self.validate()?;
        if crate::plugins::content_digest(&serde_json::to_vec(result).map_err(|e| e.to_string())?)
            != self.result_digest
        {
            return Err("The language result does not match its analysis context".into());
        }
        Ok(())
    }
}

/// LSP positions use UTF-16 code units, independently of the editor's UTF-8
/// source offsets. CR and LF are line endings, never addressable line content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Synchronize,
    /// Check a retained result without starting or substituting a service.
    ValidateAnalysis {
        context: AnalysisContext,
    },
    /// Optional speculative work. Hosts may decline it, run at most one at a
    /// time and cancel it in favor of interactive work. Never carries edits.
    Prefetch {
        kind: PrefetchKind,
    },
    PrepareAction {
        action: actions::ActionSynchronization,
    },
    ReleaseAction {
        action: actions::ActionSynchronization,
    },
    SynchronizeAction {
        action: actions::ActionSynchronization,
        close: bool,
    },
    Saved,
    Close,
    Hover {
        position: Position,
    },
    Completion {
        position: Position,
    },
    ResolveCompletion {
        id: String,
    },
    ReadResource {
        id: String,
    },
    Definition {
        position: Position,
    },
    TypeDefinition {
        position: Position,
    },
    Implementation {
        position: Position,
    },
    References {
        position: Position,
    },
    Symbols,
    WorkspaceSymbols {
        query: String,
    },
    ResolveSymbol {
        id: String,
    },
    Rename {
        position: Position,
        new_name: String,
    },
    Formatting {
        tab_width: u32,
        insert_spaces: bool,
    },
    RangeFormatting {
        range: Range,
        tab_width: u32,
        insert_spaces: bool,
    },
    CodeActions {
        range: Range,
    },
    /// Resolve only an opaque action previously returned for this source.
    ResolveAction {
        id: String,
    },
    /// Review both an action's edit and its command without executing either.
    ReviewAction {
        id: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefetchKind {
    Diagnostics,
    Symbols,
}

impl Query {
    pub fn requires_analysis_context(&self) -> bool {
        matches!(
            self,
            Self::ValidateAnalysis { .. }
                | Self::ResolveCompletion { .. }
                | Self::Rename { .. }
                | Self::Formatting { .. }
                | Self::RangeFormatting { .. }
                | Self::ResolveAction { .. }
                | Self::ReviewAction { .. }
        )
    }
    pub fn produces_analysis(&self) -> bool {
        self.method().is_some() || self.is_prefetch() || matches!(self, Self::ReadResource { .. })
    }
    pub fn is_prefetch(&self) -> bool {
        matches!(self, Self::Prefetch { .. })
    }

    /// Share ordinary synchronization/result handling after admission. This
    /// does not turn a speculative request into an interactive scheduling claim.
    pub fn foreground_query(&self) -> Option<Self> {
        match self {
            Self::Prefetch {
                kind: PrefetchKind::Diagnostics,
            } => Some(Self::Synchronize),
            Self::Prefetch {
                kind: PrefetchKind::Symbols,
            } => Some(Self::Symbols),
            _ => None,
        }
    }

    pub fn method(&self) -> Option<&'static str> {
        match self {
            Self::Synchronize
            | Self::ValidateAnalysis { .. }
            | Self::Prefetch {
                kind: PrefetchKind::Diagnostics,
            }
            | Self::PrepareAction { .. }
            | Self::ReleaseAction { .. }
            | Self::SynchronizeAction { .. }
            | Self::Saved
            | Self::Close
            | Self::ReadResource { .. } => None,
            Self::Hover { .. } => Some("textDocument/hover"),
            Self::Completion { .. } => Some("textDocument/completion"),
            Self::ResolveCompletion { .. } => Some("completionItem/resolve"),
            Self::Definition { .. } => Some("textDocument/definition"),
            Self::TypeDefinition { .. } => Some("textDocument/typeDefinition"),
            Self::Implementation { .. } => Some("textDocument/implementation"),
            Self::References { .. } => Some("textDocument/references"),
            Self::Symbols
            | Self::Prefetch {
                kind: PrefetchKind::Symbols,
            } => Some("textDocument/documentSymbol"),
            Self::WorkspaceSymbols { .. } => Some("workspace/symbol"),
            Self::ResolveSymbol { .. } => Some("workspaceSymbol/resolve"),
            Self::Rename { .. } => Some("textDocument/rename"),
            Self::Formatting { .. } => Some("textDocument/formatting"),
            Self::RangeFormatting { .. } => Some("textDocument/rangeFormatting"),
            Self::CodeActions { .. } => Some("textDocument/codeAction"),
            Self::ResolveAction { .. } | Self::ReviewAction { .. } => Some("codeAction/resolve"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    /// An editing client's unsaved-buffer identity, retained across reconnects.
    pub client: String,
    pub document: String,
    pub request: u64,
    pub revision: u64,
    pub language: String,
    /// A project-relative path; the authenticated adapter supplies the root.
    pub path: String,
    pub source_digest: String,
    pub source: String,
    pub query: Query,
}

/// Authenticated transport envelope. The server adapter derives project/root
/// from this grant; neither a plugin nor a model supplies an executable command.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub grant: String,
    #[serde(default)]
    pub session_id: Option<i64>,
    pub prepared: crate::plugins::PreparedPlugin,
    pub input: Request,
}

/// Database-validated selection. Native routes still require their normal host
/// credential and verify the selected package against that host's own receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub user: i64,
    pub project: i64,
    pub prepared: crate::plugins::PreparedPlugin,
    pub input: Request,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRequest {
    #[serde(default)]
    pub user: Option<i64>,
    pub selection: Selection,
    pub root: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revoke {
    #[serde(default)]
    pub user: Option<i64>,
    pub project: Option<i64>,
    pub provider: Option<String>,
}

pub fn declared_server<'a>(
    prepared: &'a crate::plugins::PreparedPlugin,
    language: &str,
) -> Result<&'a super::LanguageServer, String> {
    prepared.validate().map_err(|error| error.to_string())?;
    if prepared.manifest.compatibility.plugin_api < MIN_PLUGIN_API {
        return Err("Native language servers require plugin API 6".into());
    }
    prepared
        .manifest
        .contributions
        .languages
        .iter()
        .find(|definition| definition.id == language)
        .and_then(|definition| definition.features.language_server.as_ref())
        .ok_or_else(|| "This language provider does not declare a language server".into())
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err("Code-intelligence protocol version mismatch; update the app and execution bridge together".into());
        }
        self.validate_contents()
    }
    /// Durable reviews keep their original envelope. Accept archived review
    /// formats for recovery, without admitting them to live RPC.
    pub(crate) fn validate_archived(&self) -> Result<(), String> {
        if self.version != VERSION
            && !(self.version == 1 && matches!(self.query, Query::ReviewAction { .. })
                || self.version == 2
                    && matches!(
                        self.query,
                        Query::ReviewAction { .. }
                            | Query::Rename { .. }
                            | Query::ResolveAction { .. }
                    ))
        {
            return Err("Unsupported archived language review protocol".into());
        }
        self.validate_contents()
    }
    fn validate_contents(&self) -> Result<(), String> {
        if !super::text(&self.client, 256)
            || !super::text(&self.document, 256)
            || !super::identifier(&self.language)
            || !relative_path(&self.path)
            || self.source.len() > super::MAX_SOURCE_BYTES
            || super::source_digest(&self.source) != self.source_digest
        {
            return Err("Invalid code-intelligence document identity or source".into());
        }
        match &self.query {
            Query::ValidateAnalysis { context } => context.validate()?,
            Query::SynchronizeAction { action, .. }
            | Query::PrepareAction { action }
            | Query::ReleaseAction { action } => action.validate()?,
            Query::Hover { position }
            | Query::Completion { position }
            | Query::Definition { position }
            | Query::TypeDefinition { position }
            | Query::Implementation { position }
            | Query::References { position }
            | Query::Rename { position, .. } => {
                offset(&self.source, *position)?;
            }
            Query::CodeActions { range } | Query::RangeFormatting { range, .. } => {
                range.offsets(&self.source)?;
            }
            Query::ResolveAction { id }
            | Query::ReviewAction { id }
            | Query::ResolveSymbol { id }
            | Query::ResolveCompletion { id }
            | Query::ReadResource { id }
                if !super::text(id, 256) =>
            {
                return Err("Invalid code-action identity".into());
            }
            _ => {}
        }
        if let Query::WorkspaceSymbols { query } = &self.query
            && (query.len() > 256 || query.chars().any(char::is_control))
        {
            return Err(
                "Workspace symbol query exceeds its limit or contains control characters".into(),
            );
        }
        if let Query::Formatting { tab_width, .. } | Query::RangeFormatting { tab_width, .. } =
            &self.query
            && !(1..=32).contains(tab_width)
        {
            return Err("Formatting tab width must be between 1 and 32".into());
        }
        if let Query::Rename { new_name, .. } = &self.query
            && (!super::text(new_name, 256) || new_name.chars().any(char::is_whitespace))
        {
            return Err("Rename requires a nonempty name without whitespace".into());
        }
        Ok(())
    }
}

pub fn relative_path(path: &str) -> bool {
    super::text(path, 4096)
        && !path.contains(['\\', ':'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: u32,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub client: String,
    pub document: String,
    pub request: u64,
    pub revision: u64,
    pub source_digest: String,
    /// Opaque native service identity; changes after restart or provider update.
    pub server: String,
    /// Present only when the host observed complete declared project sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AnalysisContext>,
    pub capabilities: serde_json::Value,
    pub result: serde_json::Value,
    pub diagnostics: Vec<Diagnostic>,
    /// False means analysis is still pending, not that the file has no errors.
    pub diagnostics_ready: bool,
}

impl Response {
    pub fn validate(&self, request: &Request) -> Result<(), String> {
        if self.version != VERSION
            || self.client != request.client
            || self.document != request.document
            || self.request != request.request
            || self.revision != request.revision
            || self.source_digest != request.source_digest
            || !super::text(&self.server, 256)
            || self.context.as_ref().is_some_and(|context| {
                context.validate_result(&self.result).is_err() || context.server != self.server
            })
            || matches!(&request.query, Query::ValidateAnalysis { context } if self.server != context.server)
            || (matches!(request.query, Query::ValidateAnalysis { .. }) && !self.result.is_null())
            || matches!(&request.query, Query::PrepareAction {action} | Query::ReleaseAction {action} | Query::SynchronizeAction {action,..} if self.server!=action.server)
            || self.diagnostics.len() > MAX_DIAGNOSTICS
            || serde_json::to_vec(self)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_RESULT_BYTES
        {
            return Err("Code-intelligence result does not match the current request".into());
        }
        for diagnostic in &self.diagnostics {
            diagnostic.range.offsets(&request.source)?;
            if !(1..=4).contains(&diagnostic.severity)
                || diagnostic.message.trim().is_empty()
                || diagnostic.message.len() > 64 * 1024
                || diagnostic
                    .message
                    .chars()
                    .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
                || diagnostic
                    .source
                    .as_ref()
                    .is_some_and(|s| !super::text(s, 256))
                || diagnostic
                    .code
                    .as_ref()
                    .is_some_and(|s| !super::text(s, 256))
            {
                return Err("Invalid language-server diagnostic".into());
            }
        }
        Ok(())
    }
}

/// Convert only exact scalar boundaries; reject positions inside surrogate
/// pairs, beyond a row, or in either byte of a CRLF terminator.
pub fn offset(source: &str, position: Position) -> Result<usize, String> {
    let mut base = 0;
    for (line, row) in source.split('\n').enumerate() {
        if usize::try_from(position.line).ok() == Some(line) {
            let row = row.strip_suffix('\r').unwrap_or(row);
            let mut units = 0_u32;
            for (byte, ch) in row.char_indices() {
                if units == position.character {
                    return Ok(base + byte);
                }
                units += u32::try_from(ch.len_utf16()).expect("scalar uses at most two units");
            }
            if units == position.character {
                return Ok(base + row.len());
            }
            break;
        }
        base += row.len() + 1;
    }
    Err("Language-server position is outside a Unicode source boundary".into())
}

pub fn position(source: &str, byte: usize) -> Result<Position, String> {
    let prefix = source.get(..byte).ok_or("Invalid source byte boundary")?;
    let line = prefix.bytes().filter(|ch| *ch == b'\n').count();
    let row = prefix.rsplit('\n').next().unwrap_or_default();
    if row.ends_with('\r') {
        return Err("Language-server position cannot address a line terminator".into());
    }
    Ok(Position {
        line: u32::try_from(line).map_err(|_| "Too many source lines")?,
        character: u32::try_from(row.encode_utf16().count()).map_err(|_| "Source line too long")?,
    })
}

impl Range {
    pub fn offsets(self, source: &str) -> Result<std::ops::Range<usize>, String> {
        let start = offset(source, self.start)?;
        let end = offset(source, self.end)?;
        if start > end {
            return Err("Language-server range is reversed".into());
        }
        Ok(start..end)
    }
}

/// Selection policy shared by durable commands and retained workspace reviews.
pub(crate) fn validate_review_provider(
    prepared: &crate::plugins::PreparedPlugin,
    binding_revision: i64,
    input: &Request,
    registry: &super::LanguageRegistry,
    preferences: &super::LanguagePreferences,
) -> Result<(), String> {
    use super::LanguageSelection;
    let namespace = format!(
        "plugin:{}/language:{}",
        prepared.storage_namespace(),
        input.language
    );
    let provider = registry
        .find(&namespace)
        .filter(|provider| {
            provider.digest == prepared.digest && provider.installation_revision == binding_revision
        })
        .ok_or("The language provider changed after this action was reviewed")?;
    let server = provider
        .definition
        .features
        .language_server
        .as_ref()
        .ok_or("The language server is unavailable")?;
    let allowed = if server.role.is_language() {
        matches!(registry.select_server(&input.path, &input.source, None, preferences),
                LanguageSelection::Selected(selected) if selected.namespace == namespace)
    } else {
        registry
            .diagnostic_providers(&input.path, &input.source, None, preferences)?
            .iter()
            .any(|selected| selected.namespace == namespace)
    };
    if !allowed {
        return Err("The reviewed language provider is no longer selected or enabled".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
