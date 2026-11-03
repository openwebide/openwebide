# Syntax toolkit for language components

`LanguageSyntax` implements the public SDK's language protocol using reusable
incremental syntax engines. A package owns its manifest definitions and binds
each declared ID to a `LanguageGrammar`. `LanguageGrammar::standard` supplies the
existing parser families; `LanguageGrammar` also accepts a `SyntaxProvider`.
Custom languages can use the SDK directly with another engine or lexer.

Link this library into an ordinary WASI component and forward its `Plugin::language`
callback to `LanguageSyntax::handle`. Export the same definitions with
`Plugin::languages`. The toolkit handles UTF-8 full/delta source admission,
incremental trees, embedded syntax, bounded source retention, close, eviction and
version-1 token/fold/structure results. It needs no host capabilities.

The grammar descriptors live in `openwebide-language-grammars`. The shared editor
crate separates its `editor-analysis` data-consumption feature from optional
`editor-parser` implementation support used by this toolkit. Consumers must not
link the parser toolkit into browser or bridge feature implementations; the
compiled plugin owns and executes it through the public component contract.

Source releases use an immutable public Git dependency pin. The app supplies the
same public SDK source patch to first-party and community builds. C grammar builds
use the pinned WASI SDK described in [plugin authoring](../../docs/plugin-authoring.md).
