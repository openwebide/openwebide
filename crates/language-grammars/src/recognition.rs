use openwebide_plugin_sdk::languages::SyntaxLanguage as Language;

/// Pick a language from a file path's extension.
pub fn language_from_path(path: &str) -> Language {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    if matches!(
        name.as_str(),
        ".editorconfig"
            | ".npmrc"
            | ".yarnrc"
            | ".pypirc"
            | "pip.conf"
            | ".gitconfig"
            | ".gitmodules"
    ) {
        return Language::Ini;
    }
    if matches!(
        name.as_str(),
        "cargo.lock" | "poetry.lock" | "pdm.lock" | "uv.lock" | "pipfile" | "rust-toolchain"
    ) {
        return Language::Toml;
    }
    if matches!(name.as_str(), "composer.lock" | "pipfile.lock") {
        return Language::Json;
    }
    if name == ".env" || name.starts_with(".env.") {
        return Language::Ini;
    }
    let ext = path
        .rsplit(['/', '\\'])
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .filter(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" => Language::Rust,
        "py" => Language::Python,
        "js" | "mjs" | "cjs" => Language::JavaScript,
        "jsx" => Language::Jsx,
        "ts" | "mts" | "cts" => Language::TypeScript,
        "tsx" => Language::Tsx,
        "java" => Language::Java,
        "cs" => Language::CSharp,
        "php" | "phtml" => Language::Php,
        "json" | "jsonc" => Language::Json,
        "html" | "htm" => Language::Html,
        "xml" | "svg" | "csproj" | "fsproj" | "vbproj" | "props" | "targets" | "config"
        | "resx" | "slnx" | "nuspec" | "ruleset" | "runsettings" | "pubxml" => Language::Xml,
        "ini" | "cfg" | "properties" => Language::Ini,
        "css" | "scss" => Language::Css,
        "md" | "markdown" => Language::Markdown,
        "sh" | "bash" | "zsh" => Language::Shell,
        "toml" => Language::Toml,
        "yaml" | "yml" => Language::Yaml,
        "sql" => Language::Sql,
        "c" | "h" => Language::C,
        "cpp" | "cc" | "cxx" | "hpp" => Language::Cpp,
        "go" => Language::Go,
        _ => Language::Plain,
    }
}
