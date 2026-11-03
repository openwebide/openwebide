//! Grammar descriptors extend the shared parser without another editor workflow.
use openwebide_plugin_sdk::languages::{SyntaxLanguage as Language, TokenKind};
mod injections;
mod recognition;
pub use recognition::language_from_path;

pub type InjectionSelector =
    for<'tree> fn(tree_sitter::Node<'tree>, &str) -> Option<(Language, tree_sitter::Range)>;

/// Classification supplied by a grammar provider; policies remain in the shared engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxContextKind {
    String,
    Template,
    Text,
    Regex,
    Comment,
    Interpolation,
}

pub type ContextSelector = for<'tree> fn(tree_sitter::Node<'tree>) -> Option<SyntaxContextKind>;

/// Dependencies allowed when classifying an unchanged parser subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxContextScope {
    /// Node kind, flags and descendants; no absolute positions or surrounding nodes.
    Node,
    /// The same dependencies plus the immediate parent's kind.
    Parent,
    /// Arbitrary tree dependencies; classify again after every source change.
    Document,
}

/// Dependencies allowed when classifying retained syntax colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxHighlightScope {
    /// Node kind, flags and descendants; no absolute or external tree positions.
    Node,
    /// Node dependencies plus immediate-parent kind and field membership.
    Parent,
    /// Arbitrary tree dependencies; classify again after every source change.
    Document,
}

fn built_in_context(node: tree_sitter::Node<'_>) -> Option<SyntaxContextKind> {
    // Anonymous terminals such as TypeScript's `string` type are not literals.
    if !node.is_named() {
        return None;
    }
    match node.kind() {
        "string"
        | "string_literal"
        | "raw_string_literal"
        | "character_literal"
        | "char_literal"
        | "rune_literal"
        | "string_value"
        | "encapsed_string"
        | "nowdoc_string"
        | "interpreted_string_literal"
        | "interpolated_string_expression"
        | "verbatim_string_literal"
        | "raw_string"
        | "ansi_c_string"
        | "translated_string"
        | "heredoc"
        | "heredoc_body"
        | "nowdoc" => Some(SyntaxContextKind::String),
        "AttValue"
        | "PseudoAttValue"
        | "quoted_key"
        | "double_quote_scalar"
        | "single_quote_scalar"
        | "block_scalar"
        | "string_scalar"
        | "code_span"
        | "CDSect" => Some(SyntaxContextKind::String),
        "template_string" => Some(SyntaxContextKind::Template),
        "jsx_text"
        | "text"
        | "html_character_reference"
        | "entity"
        | "CharData"
        | "setting_value" => Some(SyntaxContextKind::Text),
        "regex" => Some(SyntaxContextKind::Regex),
        "comment" | "Comment" | "html_comment" | "line_comment" | "block_comment" => {
            Some(SyntaxContextKind::Comment)
        }
        "template_substitution"
        | "interpolation"
        | "string_interpolation"
        | "command_substitution"
        | "process_substitution"
        | "arithmetic_expansion"
        | "simple_expansion"
        | "expansion" => Some(SyntaxContextKind::Interpolation),
        _ => None,
    }
}

// PHP represents interpolated expressions as direct string/body children rather
// than a dedicated interpolation node. Keep delimiters and plain text opaque.
fn php_context(node: tree_sitter::Node<'_>) -> Option<SyntaxContextKind> {
    if (node.is_named() || matches!(node.kind(), "{" | "}"))
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "encapsed_string" | "heredoc_body"))
        && !matches!(node.kind(), "string_content" | "escape_sequence")
    {
        return Some(SyntaxContextKind::Interpolation);
    }
    built_in_context(node)
}

/// Custom providers use the same incremental update, limits, cancellation and fold policy.
#[derive(Clone, Copy)]
pub struct SyntaxProvider {
    pub language: Language,
    pub context: Option<ContextSelector>,
    pub context_scope: SyntaxContextScope,
    pub highlight: Option<HighlightSelector>,
    pub highlight_scope: SyntaxHighlightScope,
    pub injection: Option<InjectionSelector>,
    pub grammar: fn() -> tree_sitter::Language,
    pub fold_nodes: &'static [&'static str],
    /// Nodes whose parent supplies the visible fold header (Python suites).
    pub parent_headers: &'static [&'static str],
}

pub const SYNTAX_PROVIDERS: &[SyntaxProvider] = &[
    SyntaxProvider {
        language: Language::Rust,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "block",
            "declaration_list",
            "enum_variant_list",
            "field_declaration_list",
            "match_block",
            "use_list",
            "token_tree",
            "block_comment",
            "array_expression",
            "arguments",
            "parameters",
            "tuple_expression",
            "raw_string_literal",
        ],
    },
    SyntaxProvider {
        language: Language::JavaScript,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "statement_block",
            "class_body",
            "object",
            "array",
            "arguments",
            "formal_parameters",
            "template_string",
            "comment",
            "jsx_element",
        ],
    },
    SyntaxProvider {
        language: Language::Jsx,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "statement_block",
            "class_body",
            "object",
            "array",
            "arguments",
            "formal_parameters",
            "template_string",
            "comment",
            "jsx_element",
        ],
    },
    SyntaxProvider {
        language: Language::TypeScript,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        parent_headers: &[],
        fold_nodes: &[
            "statement_block",
            "class_body",
            "object",
            "array",
            "arguments",
            "formal_parameters",
            "template_string",
            "comment",
            "interface_body",
            "enum_body",
            "object_type",
        ],
    },
    SyntaxProvider {
        language: Language::Tsx,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        parent_headers: &[],
        fold_nodes: &[
            "statement_block",
            "class_body",
            "object",
            "array",
            "arguments",
            "formal_parameters",
            "template_string",
            "comment",
            "interface_body",
            "enum_body",
            "object_type",
            "jsx_element",
        ],
    },
    SyntaxProvider {
        language: Language::Python,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_python::LANGUAGE.into(),
        parent_headers: &["block"],
        fold_nodes: &[
            "block",
            "list",
            "dictionary",
            "set",
            "tuple",
            "argument_list",
            "parameters",
            "string",
            "comment",
        ],
    },
    SyntaxProvider {
        language: Language::Java,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_java::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "block",
            "class_body",
            "enum_body",
            "annotation_type_body",
            "interface_body",
            "array_initializer",
            "argument_list",
            "formal_parameters",
            "block_comment",
        ],
    },
    SyntaxProvider {
        language: Language::CSharp,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_c_sharp::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "block",
            "declaration_list",
            "initializer_expression",
            "argument_list",
            "parameter_list",
            "switch_body",
            "interpolated_string_expression",
            "verbatim_string_literal",
            "raw_string_literal",
            "comment",
        ],
    },
    SyntaxProvider {
        language: Language::Cpp,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_cpp::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "compound_statement",
            "field_declaration_list",
            "enumerator_list",
            "initializer_list",
            "argument_list",
            "parameter_list",
            "raw_string_literal",
            "comment",
        ],
    },
    SyntaxProvider {
        language: Language::C,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_c::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "compound_statement",
            "field_declaration_list",
            "enumerator_list",
            "initializer_list",
            "argument_list",
            "parameter_list",
            "comment",
        ],
    },
    SyntaxProvider {
        language: Language::Php,
        context: Some(php_context),
        context_scope: SyntaxContextScope::Parent,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_php::LANGUAGE_PHP.into(),
        parent_headers: &[],
        fold_nodes: &[
            "compound_statement",
            "declaration_list",
            "array_creation_expression",
            "arguments",
            "formal_parameters",
            "encapsed_string",
            "heredoc",
            "nowdoc",
            "comment",
        ],
    },
    SyntaxProvider {
        language: Language::Shell,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_bash::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "if_statement",
            "case_statement",
            "for_statement",
            "while_statement",
            "subshell",
            "compound_statement",
            "do_group",
            "heredoc_body",
            "array",
            "command_substitution",
        ],
    },
    SyntaxProvider {
        language: Language::Go,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_go::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "block",
            "field_declaration_list",
            "literal_value",
            "argument_list",
            "parameter_list",
            "raw_string_literal",
            "comment",
        ],
    },
    SyntaxProvider {
        language: Language::Html,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: Some(injections::html_injection),
        grammar: || tree_sitter_html::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &["element", "script_element", "style_element", "comment"],
    },
    SyntaxProvider {
        language: Language::Css,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(built_in_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_css::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &["block", "comment"],
    },
    SyntaxProvider {
        language: Language::Json,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(config_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_json::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &["object", "array"],
    },
    SyntaxProvider {
        language: Language::Yaml,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(config_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_yaml::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[
            "block_mapping_pair",
            "block_sequence_item",
            "flow_mapping",
            "flow_sequence",
            "block_scalar",
        ],
    },
    SyntaxProvider {
        language: Language::Toml,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(config_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_toml_ng::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &["table", "table_array_element", "array", "inline_table"],
    },
    SyntaxProvider {
        language: Language::Markdown,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(markdown_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: Some(injections::markdown_injection),
        grammar: || tree_sitter_md::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &["section", "fenced_code_block", "list", "block_quote"],
    },
    SyntaxProvider {
        language: Language::MarkdownInline,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(markdown_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_md::INLINE_LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &[],
    },
    SyntaxProvider {
        language: Language::Ini,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(config_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_ini::LANGUAGE.into(),
        parent_headers: &[],
        fold_nodes: &["section"],
    },
    SyntaxProvider {
        language: Language::Xml,
        context: Some(built_in_context),
        context_scope: SyntaxContextScope::Node,
        highlight: Some(config_highlight),
        highlight_scope: SyntaxHighlightScope::Parent,
        injection: None,
        grammar: || tree_sitter_xml::LANGUAGE_XML.into(),
        parent_headers: &[],
        fold_nodes: &["element", "Comment", "CDSect"],
    },
];

pub fn syntax_provider(language: Language) -> Option<SyntaxProvider> {
    SYNTAX_PROVIDERS
        .iter()
        .find(|provider| provider.language == language)
        .copied()
}

/// The traversal supplies the immediate parent; classifications obey highlight_scope.
pub type HighlightSelector =
    for<'tree> fn(tree_sitter::Node<'tree>, Option<tree_sitter::Node<'tree>>) -> Option<TokenKind>;

fn built_in_highlight(
    node: tree_sitter::Node<'_>,
    parent: Option<tree_sitter::Node<'_>>,
) -> Option<TokenKind> {
    // Grammar type wrappers own their complete span, including anonymous
    // terminals named `number` or `string`. Their children cannot also paint it.
    if parent.is_some_and(|parent| {
        matches!(
            parent.kind(),
            "type_identifier"
                | "predefined_type"
                | "primitive_type"
                | "integral_type"
                | "floating_point_type"
                | "boolean_type"
                | "void_type"
                | "lifetime"
        )
    }) {
        return None;
    }
    match node.kind() {
        "type_identifier"
        | "predefined_type"
        | "primitive_type"
        | "integral_type"
        | "floating_point_type"
        | "boolean_type"
        | "void_type" => return Some(TokenKind::Type),
        "tag_name" => return Some(TokenKind::Keyword),
        "attribute_name" | "property_name" => return Some(TokenKind::Attribute),
        "function_name" | "command_name" => return Some(TokenKind::Function),
        "integer_literal"
        | "float_literal"
        | "number"
        | "integer"
        | "float"
        | "decimal_integer_literal"
        | "decimal_floating_point_literal"
        | "hex_integer_literal"
        | "hex_floating_point_literal"
        | "real_literal"
        | "number_literal"
        | "int_literal"
        | "imaginary_literal" => {
            return Some(TokenKind::Number);
        }
        "true" | "false" | "null" | "none" | "None" | "True" | "False" => {
            return Some(TokenKind::Boolean);
        }
        "lifetime" => return Some(TokenKind::Lifetime),
        _ => {}
    }
    if node.child_count() != 0 {
        return None;
    }
    if parent.is_some_and(|parent| parent.kind() == "lifetime") {
        return None;
    }
    // Anonymous leaves are grammar terminals, never guessed source identifiers.
    // Their names distinguish keyword and punctuation tokens even in incomplete trees.
    if !node.is_named() {
        return Some(if node.kind() == "#" {
            TokenKind::Attribute
        } else if node
            .kind()
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch == '_')
        {
            TokenKind::Keyword
        } else if matches!(
            node.kind(),
            "." | "," | ";" | ":" | "(" | ")" | "{" | "}" | "[" | "]"
        ) {
            TokenKind::Punct
        } else {
            TokenKind::Operator
        });
    }
    let parent = parent?;
    let same = |field| {
        parent
            .child_by_field_name(field)
            .is_some_and(|child| child.id() == node.id())
    };
    let kind = parent.kind();
    if kind == "macro_invocation" && same("macro") {
        return Some(TokenKind::Macro);
    }
    if (kind.contains("function")
        || kind.contains("method")
        || kind == "call_expression"
        || kind == "call")
        && (same("name") || same("function"))
    {
        return Some(TokenKind::Function);
    }
    if (kind.contains("class")
        || kind.contains("struct")
        || kind.contains("interface")
        || kind.contains("enum"))
        && same("name")
    {
        return Some(TokenKind::Type);
    }
    None
}

fn config_highlight(
    node: tree_sitter::Node<'_>,
    parent: Option<tree_sitter::Node<'_>>,
) -> Option<TokenKind> {
    if parent.is_some_and(|parent| config_color(parent.kind()).is_some()) {
        return None;
    }
    if node.kind() == "Name" {
        return Some(
            if parent.is_some_and(|parent| parent.kind() == "Attribute") {
                TokenKind::Attribute
            } else {
                TokenKind::Keyword
            },
        );
    }
    config_color(node.kind()).or_else(|| built_in_highlight(node, parent))
}
fn config_color(kind: &str) -> Option<TokenKind> {
    match kind {
        "string"
        | "quoted_key"
        | "double_quote_scalar"
        | "single_quote_scalar"
        | "block_scalar"
        | "string_scalar"
        | "code_span"
        | "link_title"
        | "AttValue"
        | "PseudoAttValue"
        | "SystemLiteral"
        | "PubidLiteral"
        | "CDSect" => Some(TokenKind::String),
        "comment" | "Comment" => Some(TokenKind::Comment),
        "boolean_scalar" | "null_scalar" | "boolean" => Some(TokenKind::Boolean),
        "integer_scalar" | "float_scalar" => Some(TokenKind::Number),
        "bare_key" | "setting_name" | "anchor_name" | "alias_name" => Some(TokenKind::Attribute),
        "tag" | "section_name" => Some(TokenKind::Type),
        "atx_h1_marker"
        | "atx_h2_marker"
        | "atx_h3_marker"
        | "atx_h4_marker"
        | "atx_h5_marker"
        | "atx_h6_marker"
        | "setext_h1_underline"
        | "setext_h2_underline"
        | "fenced_code_block_delimiter" => Some(TokenKind::Keyword),
        "link_destination" | "uri_autolink" => Some(TokenKind::Function),
        "emphasis_delimiter" | "code_span_delimiter" => Some(TokenKind::Operator),
        _ => None,
    }
}

fn markdown_highlight(
    node: tree_sitter::Node<'_>,
    parent: Option<tree_sitter::Node<'_>>,
) -> Option<TokenKind> {
    // Prose punctuation and capitalized words remain plain. Only grammar-defined
    // Markdown constructs receive color, including delimiters and inline code.
    config_highlight(node, parent).filter(|_| config_color(node.kind()).is_some())
}
