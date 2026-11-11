//! Bounded LSP/TextMate snippets. Expansion is data, never executable code.
use std::{collections::BTreeMap, ops::Range};
mod session;
pub(crate) use session::SnippetSession;

const MAX_BYTES: usize = 256 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_FIELDS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    pub fields: BTreeMap<u32, Vec<Range<usize>>>,
    pub choices: BTreeMap<u32, Vec<String>>,
    pub transforms: Vec<SnippetTransform>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetTransform {
    pub field: u32,
    pub range: Range<usize>,
    pattern: String,
    format: String,
    options: String,
}

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Field(u32, Vec<Self>),
    Choice(u32, Vec<String>),
    Variable(String, Vec<Self>),
    Transform(String, String, String, String),
}

impl Snippet {
    pub fn expand(template: &str, variables: &BTreeMap<String, String>) -> Result<Self, String> {
        if template.len() > MAX_BYTES {
            return Err("Snippet exceeds its byte limit".into());
        }
        let nodes = Parser {
            rest: template,
            fields: 0,
        }
        .nodes(false, 0)?;
        let mut defaults = BTreeMap::new();
        defaults_for(&nodes, &mut defaults);
        let mut renderer = Renderer {
            result: Self {
                text: String::new(),
                fields: BTreeMap::new(),
                choices: BTreeMap::new(),
                transforms: vec![],
            },
            defaults,
            variables,
            stack: vec![],
            unknown: BTreeMap::new(),
            emitted: 0,
        };
        renderer.render(&nodes, 0)?;
        if !renderer.result.fields.contains_key(&0) {
            let end = renderer.result.text.len();
            renderer
                .result
                .fields
                .insert(0, std::iter::once(end..end).collect());
        }
        Ok(renderer.result)
    }
}

struct Parser<'a> {
    rest: &'a str,
    fields: usize,
}
impl Parser<'_> {
    fn take(&mut self) -> Option<char> {
        let ch = self.rest.chars().next()?;
        self.rest = &self.rest[ch.len_utf8()..];
        Some(ch)
    }
    fn nodes(&mut self, nested: bool, depth: usize) -> Result<Vec<Node>, String> {
        if depth > MAX_DEPTH {
            return Err("Snippet nesting exceeds its limit".into());
        }
        let mut nodes = vec![];
        let mut text = String::new();
        while let Some(ch) = self.take() {
            match ch {
                '}' if nested => {
                    if !text.is_empty() {
                        nodes.push(Node::Text(text));
                    }
                    return Ok(nodes);
                }
                '\\' => {
                    if self.rest.starts_with(['$', '}', '\\']) {
                        text.push(self.take().expect("checked escape"));
                    } else {
                        text.push(ch);
                    }
                }
                '$' => {
                    let braced = self.rest.starts_with('{');
                    if braced {
                        self.take();
                    }
                    let numeric = self.rest.starts_with(|ch: char| ch.is_ascii_digit());
                    let end = self
                        .rest
                        .char_indices()
                        .take_while(|(_, ch)| {
                            if numeric {
                                ch.is_ascii_digit()
                            } else {
                                ch.is_ascii_alphanumeric() || *ch == '_'
                            }
                        })
                        .last()
                        .map_or(0, |(at, ch)| at + ch.len_utf8());
                    if end == 0 {
                        if braced {
                            return Err("Snippet has an empty field".into());
                        }
                        text.push('$');
                        continue;
                    }
                    let name = self.rest[..end].to_owned();
                    self.rest = &self.rest[end..];
                    self.fields += 1;
                    if self.fields > MAX_FIELDS {
                        return Err("Snippet has too many fields".into());
                    }
                    if !text.is_empty() {
                        nodes.push(Node::Text(std::mem::take(&mut text)));
                    }
                    let index = if numeric {
                        Some(
                            name.parse::<u32>()
                                .map_err(|_| "Snippet field number exceeds its limit")?,
                        )
                    } else {
                        None
                    };
                    let node = if !braced {
                        index.map_or_else(
                            || Node::Variable(name.clone(), vec![]),
                            |index| Node::Field(index, vec![]),
                        )
                    } else {
                        match self.take() {
                            Some('}') => index.map_or_else(
                                || Node::Variable(name.clone(), vec![]),
                                |index| Node::Field(index, vec![]),
                            ),
                            Some(':') => {
                                let default = self.nodes(true, depth + 1)?;
                                if let Some(index) = index {
                                    Node::Field(index, default)
                                } else {
                                    Node::Variable(name, default)
                                }
                            }
                            Some('|') if index.is_some() => {
                                Node::Choice(index.expect("checked numeric field"), self.choices()?)
                            }
                            Some('/') => {
                                let pattern = self.segment(false)?;
                                let format = self.segment(true)?;
                                let end =
                                    self.rest.find('}').ok_or("Unclosed snippet transform")?;
                                let options = self.rest[..end].to_owned();
                                self.rest = &self.rest[end + 1..];
                                transform("", &pattern, &format, &options)?;
                                Node::Transform(name, pattern, format, options)
                            }
                            _ => return Err("Invalid or unclosed snippet field".into()),
                        }
                    };
                    nodes.push(node);
                }
                _ => text.push(ch),
            }
        }
        if nested {
            return Err("Unclosed snippet placeholder".into());
        }
        if !text.is_empty() {
            nodes.push(Node::Text(text));
        }
        Ok(nodes)
    }
    fn choices(&mut self) -> Result<Vec<String>, String> {
        let mut choices = vec![];
        let mut text = String::new();
        while let Some(ch) = self.take() {
            match ch {
                '\\' if self.rest.starts_with([',', '|', '\\']) => {
                    text.push(self.take().expect("checked escape"));
                }
                ',' => {
                    choices.push(std::mem::take(&mut text));
                }
                '|' if self.rest.starts_with('}') => {
                    self.take();
                    choices.push(text);
                    return Ok(choices);
                }
                _ => text.push(ch),
            }
            if choices.len() > MAX_FIELDS {
                return Err("Snippet has too many choices".into());
            }
        }
        Err("Unclosed snippet choice".into())
    }
    fn segment(&mut self, format: bool) -> Result<String, String> {
        let mut result = String::new();
        let mut depth = 0;
        while let Some(ch) = self.take() {
            match ch {
                '\\' if self.rest.starts_with('/') => {
                    self.take();
                    result.push('/');
                }
                '\\' => {
                    result.push(ch);
                    result.push(self.take().ok_or("Unclosed snippet transform escape")?);
                }
                '/' if depth == 0 => return Ok(result),
                '{' if format => {
                    depth += 1;
                    result.push(ch);
                }
                '}' if format && depth > 0 => {
                    depth -= 1;
                    result.push(ch);
                }
                _ => result.push(ch),
            }
            if depth > MAX_DEPTH {
                return Err("Snippet transform nesting exceeds its limit".into());
            }
        }
        Err("Unclosed snippet transform".into())
    }
}

fn defaults_for(nodes: &[Node], defaults: &mut BTreeMap<u32, Vec<Node>>) {
    for node in nodes {
        match node {
            Node::Field(index, children) => {
                defaults.entry(*index).or_default();
                if !children.is_empty() && defaults.get(index).is_some_and(Vec::is_empty) {
                    defaults.insert(*index, children.clone());
                }
                defaults_for(children, defaults);
            }
            Node::Choice(index, values) => {
                let entry = defaults.entry(*index).or_default();
                if entry.is_empty() {
                    *entry = vec![Node::Text(values[0].clone())];
                }
            }
            Node::Variable(_, children) => defaults_for(children, defaults),
            _ => {}
        }
    }
}
struct Renderer<'a> {
    result: Snippet,
    defaults: BTreeMap<u32, Vec<Node>>,
    variables: &'a BTreeMap<String, String>,
    stack: Vec<u32>,
    unknown: BTreeMap<String, u32>,
    emitted: usize,
}
impl Renderer<'_> {
    fn push(&mut self, text: &str) -> Result<(), String> {
        if self.result.text.len().saturating_add(text.len()) > MAX_BYTES {
            return Err("Expanded snippet exceeds its byte limit".into());
        }
        self.result.text.push_str(text);
        Ok(())
    }
    fn field(&mut self, index: u32, depth: usize) -> Result<(), String> {
        self.emit()?;
        if self.stack.contains(&index) {
            return Err("Snippet defaults contain a field cycle".into());
        }
        let start = self.result.text.len();
        self.stack.push(index);
        if let Some(nodes) = self.defaults.get(&index).cloned() {
            self.render(&nodes, depth + 1)?;
        }
        self.stack.pop();
        let fields = self.result.fields.entry(index).or_default();
        if fields.len() >= MAX_FIELDS {
            return Err("Snippet has too many linked placeholders".into());
        }
        fields.push(start..self.result.text.len());
        Ok(())
    }
    fn emit(&mut self) -> Result<(), String> {
        self.emitted += 1;
        if self.emitted > MAX_FIELDS {
            return Err("Expanded snippet has too many placeholders".into());
        }
        Ok(())
    }
    fn render(&mut self, nodes: &[Node], depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("Snippet expansion nesting exceeds its limit".into());
        }
        for node in nodes {
            match node {
                Node::Text(text) => self.push(text)?,
                Node::Field(index, _) => self.field(*index, depth)?,
                Node::Choice(index, values) => {
                    self.result.choices.insert(*index, values.clone());
                    self.field(*index, depth)?;
                }
                Node::Variable(name, children) => {
                    if let Some(value) = self
                        .variables
                        .get(name)
                        .filter(|value| !value.is_empty() || children.is_empty())
                    {
                        self.push(value)?;
                    } else if !children.is_empty() {
                        self.render(children, depth + 1)?;
                    } else if self.variables.contains_key(name) { /* defined empty variable */
                    } else {
                        let next = self
                            .defaults
                            .keys()
                            .chain(self.result.fields.keys())
                            .chain(self.unknown.values())
                            .copied()
                            .max()
                            .unwrap_or(0)
                            .checked_add(1)
                            .ok_or("Snippet field identity exhausted")?;
                        let index = *self.unknown.entry(name.clone()).or_insert(next);
                        self.emit()?;
                        let start = self.result.text.len();
                        self.push(name)?;
                        self.result
                            .fields
                            .entry(index)
                            .or_default()
                            .push(start..self.result.text.len());
                    }
                }
                Node::Transform(name, pattern, format, options) => {
                    let field = name.parse::<u32>().ok();
                    let value = if let Some(index) = field {
                        let mut plain = Self {
                            result: Snippet {
                                text: String::new(),
                                fields: BTreeMap::new(),
                                choices: BTreeMap::new(),
                                transforms: vec![],
                            },
                            defaults: self.defaults.clone(),
                            variables: self.variables,
                            stack: self.stack.clone(),
                            unknown: BTreeMap::new(),
                            emitted: 0,
                        };
                        plain.field(index, depth)?;
                        plain.result.text
                    } else {
                        self.variables.get(name).cloned().unwrap_or_default()
                    };
                    let text = transform(&value, pattern, format, options)?;
                    let start = self.result.text.len();
                    self.push(&text)?;
                    if let Some(field) = field {
                        self.result.transforms.push(SnippetTransform {
                            field,
                            range: start..self.result.text.len(),
                            pattern: pattern.clone(),
                            format: format.clone(),
                            options: options.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl SnippetTransform {
    pub fn apply(&self, value: &str) -> Result<String, String> {
        transform(value, &self.pattern, &self.format, &self.options)
    }
}

fn transform(value: &str, pattern: &str, format: &str, options: &str) -> Result<String, String> {
    if !options
        .chars()
        .all(|ch| matches!(ch, 'g' | 'i' | 'm' | 's' | 'u'))
    {
        return Err("Unsupported snippet regular-expression option".into());
    }
    let regex = regex::RegexBuilder::new(pattern)
        .size_limit(256 * 1024)
        .dfa_size_limit(256 * 1024)
        .case_insensitive(options.contains('i'))
        .multi_line(options.contains('m'))
        .dot_matches_new_line(options.contains('s'))
        .build()
        .map_err(|error| format!("Unsupported snippet regular expression: {error}"))?;
    let mut output = String::new();
    let mut end = 0;
    for captures in regex.captures_iter(value) {
        let matched = captures.get(0).expect("complete regex capture");
        output.push_str(&value[end..matched.start()]);
        output.push_str(&capture_format(format, &captures)?);
        if output.len() > MAX_BYTES {
            return Err("Snippet transform exceeds its byte limit".into());
        }
        end = matched.end();
        if !options.contains('g') {
            break;
        }
    }
    output.push_str(&value[end..]);
    if output.len() > MAX_BYTES {
        return Err("Snippet transform exceeds its byte limit".into());
    }
    Ok(output)
}

fn capture_format(format: &str, captures: &regex::Captures<'_>) -> Result<String, String> {
    let mut output = String::new();
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && chars
                .peek()
                .is_some_and(|next| matches!(next, '$' | '}' | '\\'))
        {
            output.push(chars.next().expect("checked escape"));
            continue;
        }
        if ch != '$' {
            output.push(ch);
            continue;
        }
        let braced = chars.peek() == Some(&'{');
        if braced {
            chars.next();
        }
        let mut index = String::new();
        while chars.peek().is_some_and(char::is_ascii_digit) {
            index.push(chars.next().expect("checked digit"));
        }
        let index = index
            .parse::<usize>()
            .map_err(|_| "Invalid snippet transform capture")?;
        let value = captures.get(index).map_or("", |value| value.as_str());
        if !braced {
            output.push_str(value);
            continue;
        }
        let delimiter = chars.next().ok_or("Unclosed snippet transform format")?;
        if delimiter == '}' {
            output.push_str(value);
            continue;
        }
        if delimiter != ':' {
            return Err("Invalid snippet transform format".into());
        }
        let mut rule = String::new();
        loop {
            match chars.next() {
                Some('}') => break,
                Some(ch) => rule.push(ch),
                None => return Err("Unclosed snippet transform format".into()),
            }
        }
        let replacement = if let Some(case) = rule.strip_prefix('/') {
            match case {
                "upcase" => value.to_uppercase(),
                "downcase" => value.to_lowercase(),
                "capitalize" => capitalize(value),
                "camelcase" | "pascalcase" => {
                    let mut words = value
                        .split(|ch: char| !ch.is_alphanumeric())
                        .filter(|word| !word.is_empty());
                    let first = words.next().unwrap_or_default();
                    let mut result = if case == "pascalcase" {
                        capitalize(first)
                    } else {
                        first.to_lowercase()
                    };
                    for word in words {
                        result.push_str(&capitalize(word));
                    }
                    result
                }
                _ => return Err("Unsupported snippet transform case".into()),
            }
        } else if let Some(yes) = rule.strip_prefix('+') {
            if value.is_empty() {
                String::new()
            } else {
                yes.into()
            }
        } else if let Some(otherwise) = rule.strip_prefix('-') {
            if value.is_empty() {
                otherwise.into()
            } else {
                value.into()
            }
        } else if let Some(conditional) = rule.strip_prefix('?') {
            let (yes, no) = conditional
                .split_once(':')
                .ok_or("Snippet conditional needs both alternatives")?;
            if value.is_empty() {
                no.into()
            } else {
                yes.into()
            }
        } else if value.is_empty() {
            rule
        } else {
            value.into()
        };
        output.push_str(&replacement);
    }
    Ok(output)
}
fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forward_links_nested_defaults_choices_and_unicode_keep_exact_ranges() {
        let result =
            Snippet::expand("$1 ${1:文${2:😀}} ${3|a\\,b,c|} $0", &BTreeMap::new()).unwrap();
        assert_eq!(result.text, "文😀 文😀 a,b ");
        assert_eq!(result.fields[&1], vec![0..7, 8..15]);
        assert_eq!(result.fields[&2], vec![3..7, 11..15]);
        assert_eq!(result.choices[&3], vec!["a,b", "c"]);
        assert_eq!(result.fields[&0], vec![20..20]);
    }
    #[test]
    fn variables_defaults_and_unknown_variables_do_not_execute_expressions() {
        let vars = BTreeMap::from([
            ("TM_FILENAME".into(), "main.rs".into()),
            ("TM_SELECTED_TEXT".into(), String::new()),
        ]);
        let result = Snippet::expand(
            "${TM_FILENAME} ${TM_SELECTED_TEXT:body} $UNKNOWN ${UNKNOWN} \\$1 \\} `echo danger`",
            &vars,
        )
        .unwrap();
        assert_eq!(
            result.text,
            "main.rs body UNKNOWN UNKNOWN $1 } `echo danger`"
        );
        assert_eq!(result.fields[&1], vec![13..20, 21..28]);
    }
    #[test]
    fn numeric_stops_end_before_text_and_unknown_variables_avoid_declared_numbers() {
        let result = Snippet::expand("$UNKNOWN $1bar ${1:foo}", &BTreeMap::new()).unwrap();
        assert_eq!(result.text, "UNKNOWN foobar foo");
        assert_eq!(result.fields[&1], vec![8..11, 15..18]);
        assert_eq!(result.fields[&2], vec![0..7]);
    }
    #[test]
    fn transforms_capture_case_conditions_and_link_updates_are_bounded_data() {
        let result = Snippet::expand("${1:hello world} ${1/(\\w+) (\\w+)/${1:/capitalize}_${2:/upcase}/} ${TM_FILENAME/(.*)\\..+$/${1:/pascalcase}/}", &BTreeMap::from([("TM_FILENAME".into(), "my file.rs".into())])).unwrap();
        assert_eq!(result.text, "hello world Hello_WORLD MyFile");
        assert_eq!(
            result.transforms[0].apply("other text").unwrap(),
            "Other_TEXT"
        );
        assert_eq!(
            transform("ab", "(a)|(b)", "${1:+A}${2:?B:X}", "g").unwrap(),
            "AXB"
        );
    }
    #[test]
    fn cycles_malformed_fields_and_excessive_expansion_fail_without_partial_text() {
        for template in [
            "${1:${1}}",
            "${1:open",
            "${1|open}",
            "${1/(/x/}",
            "${/}",
            "${1/foo/bar/z}",
            "${99999999999999999999}",
        ] {
            assert!(
                Snippet::expand(template, &BTreeMap::new()).is_err(),
                "{template}"
            );
        }
        let explosive = format!("${{1:{}}}{}", "x".repeat(1024), "$1".repeat(300));
        assert!(Snippet::expand(&explosive, &BTreeMap::new()).is_err());
    }
}
