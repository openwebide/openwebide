//! Injection selection is provider policy, independent of browser/workspace adapters.
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use openwebide_plugin_sdk::languages::SyntaxLanguage as Language;
use std::cell::RefCell;
use tree_sitter::{Node, Range};

// https://mimesniff.spec.whatwg.org/#javascript-mime-type
const JS_TYPES: &[&str] = &[
    "",
    "module",
    "application/ecmascript",
    "application/javascript",
    "application/x-ecmascript",
    "application/x-javascript",
    "text/ecmascript",
    "text/javascript",
    "text/javascript1.0",
    "text/javascript1.1",
    "text/javascript1.2",
    "text/javascript1.3",
    "text/javascript1.4",
    "text/javascript1.5",
    "text/jscript",
    "text/livescript",
    "text/x-ecmascript",
    "text/x-javascript",
];
const CSS_TYPES: &[&str] = &["", "text/css"];

fn ascii_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

pub(super) fn html_injection(node: Node<'_>, text: &str) -> Option<(Language, Range)> {
    let default = match node.kind() {
        "script_element" => Language::JavaScript,
        "style_element" => Language::Css,
        _ => return None,
    };
    let mut cursor = node.walk();
    let opening = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "start_tag")?;
    let attributes = declared_attributes(opening.utf8_text(text.as_bytes()).ok()?);
    let mime = attributes.kind.as_deref().unwrap_or("");
    let accepted = if default == Language::JavaScript {
        // HTML script preparation gives type precedence over the legacy language attribute.
        // Only classic MIME matches permit surrounding ASCII whitespace; module does not.
        if let Some(kind) = attributes.kind.as_deref() {
            kind.eq_ignore_ascii_case("module")
                || kind.is_empty()
                || JS_TYPES[2..].iter().any(|accepted| {
                    kind.trim_matches(ascii_whitespace)
                        .eq_ignore_ascii_case(accepted)
                })
        } else if let Some(language) = attributes
            .language
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let legacy = format!("text/{language}");
            JS_TYPES[2..]
                .iter()
                .any(|accepted| legacy.eq_ignore_ascii_case(accepted))
        } else {
            true
        }
    } else {
        CSS_TYPES
            .iter()
            .any(|accepted| mime.eq_ignore_ascii_case(accepted))
    };
    if !accepted {
        return None;
    }
    let mut content = node.walk();
    let range = if let Some(body) = node
        .named_children(&mut content)
        .find(|child| child.kind() == "raw_text")
    {
        body.range()
    } else {
        let mut closing = node.walk();
        let end = node
            .named_children(&mut closing)
            .find(|child| child.kind() == "end_tag");
        Range {
            start_byte: opening.end_byte(),
            start_point: opening.end_position(),
            end_byte: end.map_or(node.end_byte(), |node| node.start_byte()),
            end_point: end.map_or(node.end_position(), |node| node.start_position()),
        }
    };
    Some((default, range))
}

#[derive(Default)]
struct DeclaredAttributes {
    kind: Option<String>,
    language: Option<String>,
}

#[derive(Default)]
struct AttributeSink(RefCell<DeclaredAttributes>);

impl TokenSink for AttributeSink {
    type Handle = ();
    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        if let Token::TagToken(tag) = token
            && tag.kind == TagKind::StartTag
        {
            let mut attributes = self.0.borrow_mut();
            for attribute in tag.attrs {
                if attribute.name.local == html5ever::local_name!("type") {
                    attributes.kind = Some(attribute.value.to_string());
                } else if attribute.name.local == html5ever::local_name!("language") {
                    attributes.language = Some(attribute.value.to_string());
                }
            }
        }
        TokenSinkResult::Continue
    }
}

fn declared_attributes(opening: &str) -> DeclaredAttributes {
    // The shared HTML tokenizer handles quoted/unquoted values, entities and duplicate attributes.
    let input = BufferQueue::default();
    input.push_back(opening.into());
    let tokenizer = Tokenizer::new(AttributeSink::default(), TokenizerOpts::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}

/// Block and inline Markdown use the existing source-coordinate injection contract.
pub(super) fn markdown_injection(node: Node<'_>, text: &str) -> Option<(Language, Range)> {
    match node.kind() {
        "inline" => Some((Language::MarkdownInline, node.range())),
        "fenced_code_block" => {
            let mut cursor = node.walk();
            let info = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "info_string")?;
            let name = info
                .utf8_text(text.as_bytes())
                .ok()?
                .split_whitespace()
                .next()?;
            let language = match name.to_ascii_lowercase().as_str() {
                "rust" => Language::Rust,
                "typescript" => Language::TypeScript,
                "javascript" => Language::JavaScript,
                "python" => Language::Python,
                "csharp" | "c#" => Language::CSharp,
                "c++" => Language::Cpp,
                "bash" | "shell" => Language::Shell,
                name => crate::language_from_path(&format!("code.{name}")),
            };
            if matches!(
                language,
                Language::Plain | Language::Markdown | Language::MarkdownInline
            ) || super::syntax_provider(language).is_none()
            {
                return None;
            }
            let mut cursor = node.walk();
            let content = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "code_fence_content")?;
            Some((language, content.range()))
        }
        _ => None,
    }
}
