//! Public host code-intelligence protocol. Installed providers declare servers;
//! clients exchange source-bound data and never choose executable commands.
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
/// API 5 server declarations remain metadata. API 6 opts in to native execution
/// of the exact installed declaration, without requiring a component wrapper.
pub const MIN_PLUGIN_API: u32 = 6;
pub const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DIAGNOSTICS: usize = 4096;
pub const MAX_RECORDS: usize = super::MAX_RECORDS;
pub mod results;

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
    References {
        position: Position,
    },
    Symbols,
    Rename {
        position: Position,
        new_name: String,
    },
    Formatting {
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
}

impl Query {
    pub fn method(&self) -> Option<&'static str> {
        match self {
            Self::Synchronize | Self::Saved | Self::Close | Self::ReadResource { .. } => None,
            Self::Hover { .. } => Some("textDocument/hover"),
            Self::Completion { .. } => Some("textDocument/completion"),
            Self::ResolveCompletion { .. } => Some("completionItem/resolve"),
            Self::Definition { .. } => Some("textDocument/definition"),
            Self::References { .. } => Some("textDocument/references"),
            Self::Symbols => Some("textDocument/documentSymbol"),
            Self::Rename { .. } => Some("textDocument/rename"),
            Self::Formatting { .. } => Some("textDocument/formatting"),
            Self::CodeActions { .. } => Some("textDocument/codeAction"),
            Self::ResolveAction { .. } => Some("codeAction/resolve"),
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
        if self.version != VERSION
            || !super::text(&self.client, 256)
            || !super::text(&self.document, 256)
            || !super::identifier(&self.language)
            || !relative_path(&self.path)
            || self.source.len() > super::MAX_SOURCE_BYTES
            || super::source_digest(&self.source) != self.source_digest
        {
            return Err("Invalid code-intelligence document identity or source".into());
        }
        match &self.query {
            Query::Hover { position }
            | Query::Completion { position }
            | Query::Definition { position }
            | Query::References { position }
            | Query::Rename { position, .. } => {
                offset(&self.source, *position)?;
            }
            Query::CodeActions { range } => {
                range.offsets(&self.source)?;
            }
            Query::Formatting { tab_width, .. } if !(1..=32).contains(tab_width) => {
                return Err("Formatting tab width must be between 1 and 32".into());
            }
            Query::ResolveAction { id }
            | Query::ResolveCompletion { id }
            | Query::ReadResource { id }
                if !super::text(id, 256) =>
            {
                return Err("Invalid code-action identity".into());
            }
            _ => {}
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

#[cfg(test)]
mod tests;
