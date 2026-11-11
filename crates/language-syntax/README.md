# Syntax toolkit for language components

`LanguageSyntax` implements the public SDK's language protocol using reusable
incremental syntax engines. A package owns its manifest definitions and binds
each declared ID to a `LanguageGrammar`. `LanguageGrammar::standard` supplies the
existing parser families; `LanguageGrammar` also accepts a `SyntaxProvider`.
Custom languages can use the SDK directly with another engine or lexer.

A package can also supply its own tree-sitter grammar through `SyntaxProvider`,
using `SyntaxLanguage::Plain` as a neutral toolkit tag in both provider and
`LanguageGrammar`. Its public language IDs remain strings from the manifest.
The package owns grammar, context/highlight selectors and fold nodes; declared
`configuration.brackets` supplies pairing outside parsed strings, comments and
embedded bodies. Bracket metadata shares the editor's structural record limit.

`LanguageSyntax::handle_with` accepts an optional package refinement callback over
the reconstructed source, language ID and parsed analysis. This supports grammar
error recovery, such as unfinished HCL literals, without duplicating source/delta
retention. Only ready analysis invokes the callback. Refined data is validated
before shared bracket processing and again before publication; invalid metadata
returns an error and requires source resynchronization. Close, unsupported
languages and missing delta bases never invoke it.

Link this library into an ordinary WASI component and forward its `Plugin::language`
callback to `LanguageSyntax::handle`. Independent families construct the engine
with `LanguageSyntax::new_delegated`; embedded scopes use the selected installed
providers, including custom Markdown fenced-code IDs, through the shared document
service. Markdown's block and inline grammars run together inside its family;
only fenced-code bodies request other installed providers. Export the same definitions with
`Plugin::languages`. The toolkit handles UTF-8 full/delta source admission,
incremental trees, embedded syntax, bounded source retention, close, eviction and
version-1 token/fold/structure results. It needs no host capabilities.

The grammar descriptors live in `openwebide-language-grammars`. The shared editor
crate separates its `editor-analysis` data-consumption feature from optional
`syntax-engine` implementation support used by this toolkit. The legacy
`editor-parser` feature enables every grammar for existing editor contracts. Consumers must not
link the parser toolkit into browser or bridge feature implementations; the
compiled plugin owns and executes it through the public component contract.

Source releases use an immutable public Git dependency pin. The app supplies the
same public SDK source patch to first-party and community builds. C grammar builds
use the pinned WASI SDK described in [plugin authoring](../../docs/plugin-authoring.md).

A language-family package selects only the grammars it owns. For example:

```toml
openwebide-language-syntax = { git = "https://github.com/openwebide/openwebide.git", rev = "<immutable-release-commit>", default-features = false, features = ["javascript-typescript"] }
```

The family features are `rust`, `javascript-typescript`, `python`, `java`,
`csharp`, `c-cpp`, `php`, `shell`, `go`, `html`, `css`, `json`, `yaml`, `toml`,
`markdown`, `ini`, `xml`, `sql` and `plain`. Related dialects share a package
according to the intended language server's coverage. `default-features = false`
is required for independently packaged families; the compatibility default `all`
retains the complete grammar set. SQL and plain text do not link a parser grammar.
These features do not start a language server.

Run `python3 tools/check-language-isolation.py` from the app repository to verify
that each family resolves only its own parser dependencies.
