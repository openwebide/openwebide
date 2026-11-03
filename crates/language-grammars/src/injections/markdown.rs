use openwebide_plugin_sdk::languages::SyntaxLanguage as Language;
use tree_sitter::{Node, Range};

/// Block and inline Markdown use the existing source-coordinate injection contract.
pub(crate) fn markdown_injection(node: Node<'_>, text: &str) -> Option<(Language, Range)> {
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
            ) || crate::syntax_provider(language).is_none()
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

pub(crate) fn markdown_language_injection(node: Node<'_>, text: &str) -> Option<(String, Range)> {
    if node.kind() == "inline" {
        return Some(("markdown-inline".into(), node.range()));
    }
    if node.kind() != "fenced_code_block" {
        return None;
    }
    let mut cursor = node.walk();
    let info = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "info_string")?;
    let name = info
        .utf8_text(text.as_bytes())
        .ok()?
        .split_whitespace()
        .next()?
        .to_ascii_lowercase();
    let language = match name.as_str() {
        "rust" => "rust".into(),
        "typescript" => "typescript".into(),
        "javascript" => "javascript".into(),
        "python" => "python".into(),
        "csharp" | "c#" => "csharp".into(),
        "c++" => "cpp".into(),
        "bash" | "shell" => "shell".into(),
        name => match crate::language_from_path(&format!("code.{name}")) {
            Language::Plain => name.to_string(),
            language => format!("{language:?}").to_ascii_lowercase(),
        },
    };
    if language.len() > 64
        || !language
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
        || !language
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, b'-' | b'_'))
    {
        return None;
    }
    let mut cursor = node.walk();
    let body = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "code_fence_content")?;
    Some((language, body.range()))
}
