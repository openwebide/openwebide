# Editor controls

Edit runs in Rust/WebAssembly and uses the same commands and file policies for
local and remote projects. The full editor roadmap is still in progress.

## Appearance and tabs

Settings → Editor defaults offers all five bundled Monaspace families: Neon (the
default), Argon, Xenon, Radon, and Krypton. Texture healing and coding ligatures
are independent toggles, both enabled by default. These preferences sync through
your account and apply to Edit and diff views; fonts are available offline in the PWA. Font family and OpenType changes invalidate source geometry even when the browser cannot serialize the font shorthand.

Right-click or long-press a project or file tab for **Close others**, **Close all
to left/right**, and **Move left/right**. File tabs also expose the tree's file,
Git and chat actions, even when the Files dock is collapsed. Bulk file closes
confirm unsaved changes together and reject changes made while confirmation is
open. The persistent project-less Chat tab remains available.

## Typing and editing commands

- Tab advances to the next indentation stop; Shift+Tab outdents selected lines.
- The folding gutter keeps a fixed width while syntax detection updates, so typing
  and pressing Enter do not shift the text horizontally.
- Enter retains indentation and uses the configured line ending, or the file's
  first line ending when no rule is set. Supported code languages indent after an
  opening bracket; Python also indents after a code colon. Enter inside an empty
  bracket pair puts the closer on its own line. Strings and comments are opaque.
- Typing a closing bracket on an otherwise empty indented line aligns it with its
  opener. Brackets and language-supported quotes auto-close in code, wrap selected
  text, and skip an existing closer. Backspace between an empty pair removes both.
  Rust apostrophes are inserted without auto-closing so lifetimes remain ordinary
  typing; selected text can still be wrapped in character quotes. Comments and
  strings retain ordinary typing behavior.
- Ctrl+Z / Cmd+Z undo; Ctrl+Shift+Z / Cmd+Shift+Z redo (Ctrl+Y also works).
- Ctrl+M toggles whether Tab indents or moves keyboard focus out of the editor.
  The native input is named for its file, describes the current Tab behavior,
  and politely announces changes to this keyboard mode.

The editing menu provides line and comment commands and **Reindent selected
lines**. Keyboard equivalents:

| Command | Shortcut |
| --- | --- |
| Move selected lines | Alt+Up / Alt+Down |
| Duplicate lines above/below | Alt+Shift+Up / Alt+Shift+Down |
| Duplicate selected text (or current line) | Ctrl/Cmd+Shift+D |
| Delete selected lines | Ctrl/Cmd+Shift+K |
| Insert an indented line above/below | Ctrl/Cmd+Shift+Enter / Ctrl/Cmd+Enter |
| Toggle line/block comments | Ctrl/Cmd+/ / Ctrl/Cmd+Shift+/ |
| Paste and match indentation | Ctrl/Cmd+Shift+V |

Normal paste preserves clipboard whitespace. Matching indentation is explicit:
remove the snippet's common leading indentation, preserve relative indentation,
and rebase subsequent lines to the receiving line. Whitespace-only paste is kept;
configured line endings apply to inserted text. Paste and reindent each form one
undo step. Reindent aligns selected bracket-delimited blocks, leaves multiline
string contents untouched and preserves Python's existing block depth; it is not
a language formatter. Unsupported comment/reindent actions appear disabled.

Reindent also works inside HTML script/style bodies and supported Markdown code
fences. If code structure is still being prepared, the requested action waits for
it. Changing the source, selections, indentation rules, file or account cancels
that request. Unavailable structure reports an error without changing the file.

## Multiple selections and clipboard

With multiple selections, Copy joins their source text in primary-selection order;
Cut removes those ranges in one undo step after writing the clipboard. Paste puts
one clipboard line into each selection when the line and selection counts match;
otherwise it repeats the complete text at every selection. Copy also includes compact
selection metadata: when it survives the clipboard and the cursor counts match,
each cursor receives its original fragment, including multiline and empty fragments.

Paste and match indentation rebases each fragment at its receiving line. Changed,
invalid or stripped metadata uses the plain-text behavior above. CRLF separators
between distributed plain-text lines are removed from their bodies. Clipboard
failures leave the source unchanged. Clipboard edits are disabled during an active input composition.

Use **Editing commands** or these shortcuts for multiple selections:

| Action | Shortcut |
| --- | --- |
| Select word, then next occurrence | Ctrl/Cmd+D |
| Select all occurrences | Ctrl/Cmd+Shift+L |
| Add cursor above/below | Ctrl/Cmd+Alt+Up/Down |
| Expand/shrink selection | Alt+Shift+Right/Left |
| Keep primary cursor | Escape |

Column gestures retain their anchor, tab geometry and source-version identity,
without retaining file text. The shared document engine reuses indexed logical
rows for rectangles; edits, reloads, account/project changes and indentation
changes reject stale drags. Standalone column queries use the same validation
and grapheme policy with a fresh row index.

Alt-click adds/removes a cursor. Alt+Shift click/drag selects a column from the
primary anchor, honoring tab stops and complete Unicode graphemes. Secondary
carets and selections use the same font metrics as the syntax paint, including
wrapped text; screen readers receive the cursor count. Arrow keys move all cursors
by grapheme or logical line; with word wrap enabled, Up/Down use measured visual
rows.

Ctrl/Alt+Left/Right move by word, and Shift extends
each selection. Home/End move to line boundaries, Ctrl/Cmd+Home/End to document
boundaries; on macOS Cmd+arrows use line/document boundaries. Vertical movement
retains the desired column across short lines, using pixels for wrapped rows.
The shared engine retains soft-wrap affinity, skips folded rows and resets its
horizontal goal after width/font changes.

Measured primary and secondary carets
share the existing selection overlay so a wrap boundary stays on the selected row.
Measurements bind to the complete source and fold projection; stale/missing
coverage rejects the entire movement without changing text, selections or history.
The DOM adapter indexes bounded logical lines by grapheme byte/UTF-16 coordinates,
locates visual rows with binary DOM range searches and measures only current and
neighboring visual rows.

Oversized paint tokens use Unicode-safe text runs so
range measurements stay within short text nodes. It reuses measurements across
cursors and caps prepared
caret positions at 65,536. While paint is pending, arrow requests queue in order, including
Shift selections. Typing, commands, composition and clipboard actions apply queued motion first.
The shared facade borrows the current source when scheduling arrows/pages. Queues
retain document identity, revision, selections and their immutable projection
without another full-source copy. Replacing/recovering a document invalidates its
old queue even when the replacement has identical text and revision; a clone of
the same text version shares identity, while editing either clone creates a new
identity. Divergent clones cannot accept each other’s queued motion even with
matching revisions and selections.
When a full row-height table is still preparing, styled probes measure the current
and neighboring logical lines using the same caret sampler as prepared paint.

Explicit neighbor links preserve wrapped movement without estimating unmeasured
heights; stable line-relative IDs are mapped separately to screen coordinates. File/account/source/fold changes cancel stale
requests; unavailable layout cancels after eight frames with an error. Full paint
viewport preparation remains a follow-up. Basic single/multiple-cursor movement
and queued page/arrow navigation work throughout admitted editor files (up to
8 MiB, 100,000 display lines and 1 MiB per line). They reuse indexed logical rows;
measured neighborhoods share the document source and validate grapheme positions
only in measured rows, using prepared sparse coordinates for long rows. Structural
selection commands retain their separate 2 MiB analysis budget; Escape still
returns to the primary cursor in larger files.

## Indentation

The app footer shows one **Spaces: 4** / **Tabs: 4** control. Open it for separate
indentation and tab widths, plus detection/EditorConfig details.
These are separate: an indentation step can be four columns while a hard tab
occupies three. Tabs fill as many complete tab stops as possible, then spaces fill
the remainder. Changing these controls affects the current document's commands and
how tabs are displayed. **Convert indentation**, in the editor **…** menu or
Search, explicitly rewrites leading
whitespace while retaining its displayed column width. Conversion is one undo step;
it does not format the rest of the code. Widths are bounded to 1–16 columns.

Set user defaults in **Settings → Editor defaults**. Changes save immediately to the
user's database settings. Per-file footer overrides remain with the open document
for the current app lifetime; they do not change the project's `.editorconfig`.

## File rules

The editor discovers `.editorconfig` files from the file's parent directory up to
the project root. It stops at `root = true`; it does not read outside the workspace.
Closer files and later matching sections override earlier rules. `unset` removes
a property. When no rule applies, existing indentation is detected, then user
defaults fill in missing values.

Supported editing/save properties are `indent_style`, `indent_size` (including
`tab`), `tab_width`, `end_of_line` (`lf`/`crlf`), `insert_final_newline` and
`trim_trailing_whitespace`. Charset conversion is not performed; Edit requires
UTF-8. Glob sections support paths, wildcards, alternatives and integer ranges.

Without explicit save rules, line endings, trailing whitespace and the presence
or absence of a final newline are preserved. Configured cleanup is applied as one
undoable command before writing. Empty documents never gain a final newline.
Undoing cleanup after a successful save marks the buffer dirty again.
An in-flight save can finish after switching file or project tabs. Its acknowledgement
updates only the original account, project and folder; a changed folder/bridge,
removed project or account change cannot mark a replacement draft saved. Edits made
while writing remain dirty, and a failed write preserves the draft and saved baseline.

Unreadable, invalid or oversized configuration files produce a notice and fall
back to the remaining rules, detected indentation and defaults. Configuration
files are limited to 256 KiB. Saving `.editorconfig` refreshes the active file's
rules; switching files rediscovers rules. A pending discovery is discarded after
a project, folder, bridge or account change.

## Syntax and language behavior

Paired typing/deletion, Enter, selected-line reindent, line/block comments,
structural selection expansion and bracket navigation use validated parser
contexts when available, including JavaScript template interpolation and HTML
script/style bodies. Reindent keeps multiline literal content unchanged and
separates embedded bodies, including after an unclosed block. Block comments use
the language at each selection; caret edits stay within an embedded body, and
selections escaping that body leave the document unchanged.

Line comments combine
per-language line markers and block-comment fallbacks in one transaction, deduplicate
identical targets and preserve caret/selection direction through undo/redo.
Selection expansion follows parsed words, expressions, blocks and functions
before whole-line fallbacks, with validated source ranges and reversible shrink
history. Bracket navigation and its decorations use the same contexts, including
interpolation code and separate embedded bodies.

Editing retains bounded structural fallbacks when a parser is unavailable.
A compact spinner occupies a reserved final slot in the status bar while the
current file is being prepared. Its “Preparing syntax…” tooltip and accessible
label disappear with the spinner when colors or terminal plain fallback are ready;
the existing status controls keep their positions.
Syntax colors stay plain until grammar preparation succeeds; unsupported files
never receive guessed code colors. Edit highlighting uses the same cached
providers, with extensible highlight selectors and parser-protected literal/comment
spans, including interpolation code and HTML script/style bodies. Tokens preserve
source bytes; the DOM adapter only normalizes CRLF for textarea alignment.
Highlight selectors receive the node and its cursor-provided immediate parent
(`None` for a tree root), preserving declared node/parent/document dependencies
without searching backward through wide sibling lists. Retained syntax parts
use that same parent context for identity and dependency checks.
Inline/Split diffs, recovery reviews, Git previews and chat diff previews parse
each complete source version with the shared providers, then intersect syntax
colors with word-change boundaries.

Source line numbers remain independent of
alignment gaps; CRLF terminators are excluded while a final bare CR is preserved.
PHP heredocs/nowdocs and shell heredocs preserve literal contents. PHP interpolated
expressions (including their braces), shell command/parameter/arithmetic substitutions,
Python f-strings and C# interpolation expose code while keeping nested literals
protected, including through enclosing string wrappers.
Files over 2 MiB or 65,536 bracket tokens fall back to ordinary indentation and
typing. Large-file benchmarks stay on the roadmap.

The edit view retains each file’s caret, selection direction and horizontal/vertical
scroll position within its project while the app is open, including when switching
to a diff view and back. New files start at the beginning; changing accounts clears
these positions. Database recovery also restores these positions on reload, subject to the current file’s scroll bounds.

The syntax/folding foundation uses shared incremental grammar providers for Rust,
TypeScript/TSX, JavaScript/JSX, Python, Java, C#, C++, PHP, Shell, C, Go, HTML and
CSS. Trees update with Unicode-safe byte edits. Fold descriptors cover blocks,
declarations and multiline comments/literals, with Python suite headers retained.
Cancellation and oversized files discard stale trees; unsupported languages use
lexical/indentation folding. Custom `SyntaxProvider` descriptors use the same
update and fold policy through `SyntaxDocument::with_provider`.

Frontend builds enable the core `editor-parser` feature. Backend WASI builds do not
need the parser or a WASI C SDK. A shared browser compiler adapter supplies portable
C headers; see `vendor/tree-sitter-language/PATCH.md`. The existing Rust grammar
patch remains compatible with that adapter.

HTML script/style bodies use independent JavaScript/CSS parsers and full-file
fold coordinates, including after Unicode/CRLF edits. Declared types select
supported bodies; JSON data scripts and unsupported style types stay in HTML.
Each provider can supply an injection selector returning validated source ranges.
The shared engine limits a document to 64 embedded bodies and discards every tree
on cancellation or an exceeded limit.

Editing commands, structural selection expansion and bracket navigation use
immutable parser contexts from the shared per-document cache. `SyntaxDocument::prepare`
publishes one source-bound snapshot of folds, structure and tokens per source/tab
width. Tab-width changes retain structure and token allocations for unchanged source,
recomputing indentation-dependent folds. Consumers share immutable allocations;
source updates and cancellation invalidate the cache while previously published
snapshots retain their original source.
Parser folds retain descriptors for unchanged top-level subtrees. The previous
tree keeps node IDs alive; reused positions rebase from the new subtree origin,
closing-line text is checked again and reused nodes still count toward analysis
limits. External parent-owned headers use fresh extraction. Each embedded parser
retains its own bounded current-tree cache, cleared with parser cancellation.
Editing contexts use the same retained-tree identity and visit budget, with
source-relative protected regions, interpolation holes and selection ranges.
Classifiers declare `SyntaxContextScope::Node` (kind, flags and descendants),
`Parent` (also the immediate parent's kind) or `Document` (broader dependencies).
Document-dependent classifiers and chunks whose interpolation owners lie outside
the subtree use fresh extraction. Built-in PHP classification uses the parent
scope; other built-ins use node scope. Context assembly and lexical fallbacks
still scan the complete source, and edits inside a large top-level subtree still
revisit that subtree.
The frontend facade also rejects changed account, file-read, source and tab-width
scopes before publishing results.

Each cursor uses its own language, including JavaScript/CSS
in HTML, and template interpolation code remains editable while literal text is
protected. Incomplete input uses bounded lexical fallback; JavaScript template
interpolation also works during incomplete typing. Contexts verify their exact
source before a command can mutate history. Custom provider classifiers use the
same traversal, limits and fallback policy.

## Preparation and viewport rendering

The production editor runs preparation in a dedicated module worker using the
same Rust/WASM build. The browser adapter handles messages, startup readiness,
timeouts and termination; the shared Rust engine owns analysis, validation and
cache policy. One request runs at a time and one latest source waits, so repeated
typing replaces queued work. Replies must match the exact source, file/project,
account generation, read revision, epoch and tab width before publication.

UTF-8 ranges, folds, token coverage and bracket links are validated without parsing
the document again on the UI thread. Transport/startup failures use the same
preparation engine synchronously with a 12 ms parser budget; unavailable contexts
retain ordinary lexical editing. Worker parsing has a 100 ms budget.
Explicit Reindent requests use a yielding browser task adapter for the same Rust
syntax service when no current structure is ready, then apply through the shared
editor command facade. They do not use the immediate lexical fallback. Other
parser-aware commands still retain that fallback while their readiness policy
remains on the roadmap.
JSON/JSONC, TOML, YAML/YML, INI/EditorConfig, XML build configuration and Markdown
use registered Rust/WASM grammars through the same preparation cache and worker.
Markdown has separate block/inline parsing and declared fenced-code languages;
ordinary prose stays plain. File detection recognizes common manifest/settings
names including MSBuild projects/solutions, NuGet.Config, pom.xml, setup.cfg,
Cargo/Python lockfiles, Pipfile, Composer lockfiles, Git config, npmrc and env files. SQL and other unsupported grammars retain plain paint.

Cooperative plain-row fallback preserves source boundaries and cancellation
publishes no partial rows. LICENSE, NOTICE and other extensionless prose stay plain.

Unwrapped edit views render an overscanned row window for syntax, line-number
gutters and fold controls. The shared row-window policy receives browser geometry;
projected rows retain global native UTF-16 offsets for Unicode/CRLF pointer mapping.
Syntax tokens and indentation guides are reused across scrolling. Paint and cursor
measurements synchronize native viewport dimensions as well as scroll offsets,
so a delayed resize/scrollbar observer cannot move a cursor using stale wrap width.

Find and navigation
can reveal rows outside the current paint window. Wrapped views and rows containing
standalone CR also use exact measured row-height windows. Cold measurement and
remeasurement use temporary batches of at most 128 logical rows and 64 KiB of
source, keeping each logical row intact. An individually longer row may exceed
that byte budget up to the admission cap; finer rendering within these rows
remains a follow-up.

Batches release their DOM before yielding, with a frame turn
every eight batches. The native input stays visible and editable until exact
measurements are ready. Shared progress permits queued cursor movement to wait
while preparation advances, retaining a bounded retry for stalled work.
Measurement publication checks source, folds, project, account and font/width
scope, and rejects a height table that disagrees with native scrolling.

Offscreen
cursor-neighbor probes measure exact geometry on demand in temporary row/byte
batches; no hidden neighbor paint is retained between key presses. Both-mode
browser contracts move 60 distant cursors and restore their exact selections,
check the actual probe limits and release, and keep persistent paint in its viewport
window. Cursor motion preserves the projection revision when folds do not
change.

Resize, font and styled-paint changes require fresh measurements; collapsed
panels cancel pending work and restart when visible. Localized edits reuse exact
heights for unchanged styled prefixes and suffixes, including row insertions,
deletions and undo. Reuse checks account, project, file-read, font/width,
indentation, whitespace, token styles and line-ending scope; font changes clear the
cache. Large regions between disjoint edits reuse matching interior rows through exact
paint equality as well; conflicting measurements for identical rows are not reused.

Browser contracts compare exact source and selections after queued arrows followed
by typing, composition, paste and cut with cold versus prepared layouts in both
workspace modes. Real-device input and clipboard verification remain follow-ups.
Cancellable, noncomposing text insertion records event data before the browser's
native edit, then commits through the shared document transaction engine after
validating source, read, account, selection and folded projection ownership.

Small unbound single-cursor insertion keeps the native textarea value; trusted
browser commits without folds also skip the input handler's full DOM-value read.
Folded, multiple-cursor, synthetic and fallback input reconcile the installed
value, using scoped surrounding text when a native window is bound. Multiple
cursors use the same typing-history group. Typing declarations retain a bounded
16 KiB surrounding-text projection around the selected range start. Local byte and
native offsets map to full document coordinates, including partial rows, Unicode,
CRLF and folded gaps; replacing a larger directional selection still edits its
complete source range.

Native origins and source/read/account/document/selection
checks reject stale declarations. This bounds the retained declaration, not the
textarea value or layout. The shared editor facade also captures immutable native
contexts with 12 KiB of surrounding text and 4 KiB of growth headroom. Context
replay borrows the source and preserves the complete selection even when the
browser can hold only part of it, including directional Unicode/CRLF replacements
and multiple cursors.

Source, selection, fold, document, read, project and account
changes reject stale contexts. After a commit, the facade rebases the context to
retain a matching browser value; composition can retain its growing value under
the document admission limits instead of replacing active input-method text.
Prepared larger editors on fine-pointer devices now bind these contexts to the
native textarea. Binding state belongs to the workspace, so independently created
facades and capture-in-chat commands use the same installed context. Source paint,
clipboard and keyboard commands retain full-source
coordinates; Select All and Page Up/Down operate on the document rather than the
clipped window. Leaving Edit view releases the native binding; a remounted input
uses full-source geometry until its prepared source extent is ready again. A
generation stamp is installed after native value/selection
restoration, rejecting events from older windows. Composition ownership includes
read revision and account, so stale previews cannot restore snapshots into new
reads or accounts. Provider metadata retains the native projection when collapsed
ranges are unchanged. A bounded native probe positions the browser composition
caret at the source paint point without changing the source viewport. Small files
retain the trusted insertion shortcut. Initial cold layout and coarse/touch devices
retain full native input until source caret, extents and touch selection are ready;
physical Chrome/Edge PWA input-method verification remains pending.
Document and composition snapshots share source text, logical-line indexes and
prepared projections. A changed version detaches once with insertion headroom;
ordinary subsequent edits reuse its buffer. Cancellation restores the committed
version and its prepared projection. Recovery serialization still copies the
committed source at the persistence boundary, and editing still shifts suffix
bytes/indexes and rebuilds changed projections.
Pointer selection adapters borrow current source while gestures retain the shared
document version identity. Replacing a document invalidates its gestures even when
text and revision match; unchanged document snapshots remain valid.
`tools/check-editor-pointer.py` uses trusted Chromium mouse events to check token
columns, far-right blank space and scrolled bounded Rust/C#/JSON drafts with
press/move/release and insertion/undo in both workspace modes. Click and drag hits
validate browser positions against measured text boundaries, including token and
container fallbacks, and normalize vertical row padding to the glyph band. It does
not prove physical touch selection or installed-PWA behavior. The verifier awaits
editable, source-owned paint, completed recovery checks and loaded fonts before measuring gesture coordinates;
failure diagnostics retain row/input rectangles and event cancellation state.

While worker syntax is pending, cold views borrow plain row bodies from the existing
projection instead of normalizing and tokenizing another complete file on the UI
thread. Requested rows and source slices feed the same paint and measurement paths.
An existing styled frame stays visible within its document/read/account scope;
replacement batch measurements wait for syntax, and source pointer controls remain
disabled while its paint is stale. Forced caret probes can still reconcile current
source. Terminal and unavailable-worker fallback preserves plain source in
cooperative whole-row batches, publishing only complete current-source tokens.
Plain-row updates reuse matching raw source in both the worker and fallback paths. Fallback preparation reuses a current published worker
scope. Syntax queries still copy source in some paths.
Cold source geometry, long-row shaping and touch input remain unfinished.

Frontend app and browser-test links reserve a 2 MiB WASM stack for nested Leptos
views. The editor erases its outer view type to reduce return-value copying;
source buffers and admission limits remain independent of this stack allocation.

Prepared primary caret/selection paint and single-cursor
keyboard motion use shared source geometry and Rust commands. Caret reveal uses
the common scroll viewport, including offscreen rows. Prepared mouse clicks resolve measured character positions and clamp blank
space past the text to the line end. Prepared clicks and
drags select source carets, words or logical lines through the same
Rust policy; Shift-click preserves the existing anchor.

Copy/cut use full source
ranges for single selections too, preserving CRLF and shared undo behavior.
Holding a drag near or beyond a viewport edge scrolls and extends the source
selection vertically or horizontally, including with a stationary pointer. Mouse
release or window blur stops it; pending paint is retried. Source/read/project/
account and fold changes discard active drags.

Prepared source row
widths/heights now control scroll extents, reusing unchanged row dimensions after
edits and retaining the source viewport's trailing padding when native scrolling
clamps earlier. Source/account/syntax/layout guards discard obsolete dimensions; prepared
scrolling reads no native input width/height. Physical browser size limits still
apply. During prepared edits, the source viewport retains its last measured
extents until replacement measurements arrive; local native scrolling cannot
change that viewport. Cold/touch selection, initial cold extents and cold caret
ownership still need to move out of full native input.

Direct insertion and native replay share newline normalization; rejected edits
preserve source and selections. Non-cancellable input and IME map projected changes
into source replacements and validate their resulting selections against borrowed
prefix/insertion/suffix pieces. Replay constructs no complete replacement value;
complete textarea diffing compares borrowed raw chunks between normalized CR/LF
boundaries and returns source byte boundaries directly, without constructing normalized source copies or
rescanning prefixes for UTF-16 conversion. Duplicate composition commits use the
same borrowed comparison and leave selections/history unchanged. Complete-value
comparison remains linear in unchanged prefix/suffix text. Chunks retain complete
Unicode characters and never split a CRLF pair; normalization at chunk edges uses
the same directional character iterator as native text generation. Cold geometry
probes, cropped row paint and visual cursor measurements use this borrowed check
to validate DOM text without allocating temporary normalized source strings.
Ambiguous repeated-text edits retain the original selected occurrence. IME previews
validate eventual secondary edits against the same byte, line and long-line limits
before publishing the primary change, and failed
frames restore the pre-composition source, selections and redo history.

Composition cancellation restores the committed document before publishing. Its
discarded preview and restored source stay borrowed during ownership checks; only
matching active views and retained project snapshots copy the restored source.
The engine still retains a full committed source baseline while composition is active.
Document clones share immutable history steps and transaction payloads. Appending
a shared typing group copies its step metadata and transaction references, retaining
the existing replacement strings; undo/redo cursors and pruning remain independent.
Saved-text baselines are also immutable and shared by clones. A save acknowledgement
retains the written version separately from newer edits, shares it with an active
composition baseline and reuses an identical saved version. Recovery still serializes
owned text at the persistence boundary.

Ownership checks, cursor counts, Select All and navigation queries borrow current
source instead of cloning complete file values. Native selection mapping lives in
the shared input facade; the DOM adapter supplies UTF-16 positions and reads its
value only when an unbound folded view needs validation. Ordinary typing keys do not
capture a source snapshot in selection dispatch. Commands, clipboard edits and
search replacements publish the document's immutable source handle and return
selections; native input and composition completion publish the same handle.
Active content, retained file buffers, project snapshots and Find scopes share
immutable editor text. Hydration and document initialization share source too;
edits detach retained versions before mutation. Write and recovery transfers
retain their existing owned-string contracts. Matching borrowed source validates
pointer/length first, with complete byte equality for external text.
Ordinary transactions validate borrowed proposed pieces before mutating the existing
String, including grouped undo/redo. Growth reserves at most 64 KiB of headroom
above the transaction's peak size, rather than doubling a large buffer. Affected row contexts are merged so multiple
cursors rebuild a shared row once, while distant edits retain interior coordinates.

Fold rebasing uses precise edited boundaries and retains unaffected collapsed ranges
until providers refresh. Admission reuses indexed break counts for unchanged complete
rows in already admitted sources, scanning inserted text and joining boundary rows.
Non-admitted source retains the full scan and its error order. The index adds a break
prefix and oversized-row flag per logical row; suffix updates still shift those
prefixes. Embedded-language range validation shares the parser edit path’s indexed point
mapping, including Unicode, CRLF and EOF after row insertions/deletions.
Syntax scopes, lexical jobs and shared parser preparation retain the immutable
editor source; validated worker replies retain the same snapshot. Workers retain
resolved request strings directly. Borrowed external parser calls, Git diff shaping,
serialization and normalized native text still materialize source Strings. The native textarea still owns
the complete projected source during initial cold preparation and on touch devices;
prepared fine-pointer editors retain surrounding text instead. Remaining source
materialization and retained versions still prevent complete viewport memory bounds.

## Document coordinates and caches

The shared document maintains logical-line and UTF-16 prefixes across transactions,
grouped undo/redo and composition. Commands reuse indexed rows; native caret mapping
binary-searches the appropriate row. Long rows retain sparse, Unicode-safe
byte/native-UTF-16/character checkpoints every approximately 512 bytes. Document
and folded-view offset queries and cursor line/column status scan only the tail
after a checkpoint. Rows outside the rebuilt edit region retain their indexes;
folded views share the same immutable checkpoints.

Parser byte coordinates use an incremental logical-line table with the same
changed-row reconstruction and suffix rebasing as document coordinates. Edit
points query indexed rows, and grammar paint reads those row boundaries rather
than splitting the complete source again. Direct parser updates enforce the
existing row limit; cancellation clears the retained coordinates. Shifted suffix
records still require rebasing.

Parser fold validation queries the same indexed rows without building a complete
list of row slices. The shared fold policy borrows those rows for grammar-backed
documents; standalone lexical callers index once. Syntax row slices retain CR
before LF so Tree-sitter byte columns and closing-line sibling checks keep their
existing meaning. Lexical scans and fold-result assembly still visit the file.

Grammar paint assembles one row at a time, comparing incoming piece identities
against that row's retained list. Matching rows keep their list and token
allocations; changed classifications rebuild the row. This avoids temporary
piece lists for the complete source, while boundary construction and the final
row table still visit the file.

Ready unwrapped frames can bind bounded native surrounding text while complete
row measurement is still pending. The shared input facade validates the current
frame's source revision, complete native value and dimensions; the browser captures its full-source scroll extents
before replacing that value. Restored windows publish their ownership stamp
before scroll reconciliation, preserving the complete file's viewport. This
transition rejects wrapping and active composition. Before a ready frame, and for
cold wrapped or touch input, the full native surface remains the fallback.

Lexical scanning returns source-independent structural metadata. Parser fallback
and lexical folding consume this borrowed scan without copying complete files or
embedded-language bodies into temporary structures. Owned editing contexts use
the same scanner and region queries, including the byte/bracket limits and
unavailable-context fallback. Scanning and metadata assembly still visit the source.

Long-row paint and cursor probes
also share sparse grapheme/UTF-16 coordinates and cached horizontal eligibility;
unchanged rows retain their allocation. Lookups scan from exact cluster boundaries
instead of preparing a complete glyph array on each probe. Validated fragment paint is retained for revisited intervals (at most 16 entries
and 2 MiB of HTML), scoped by document, account, read/pending ownership, projection,
syntax/guide allocation, indentation, whitespace and browser shaping/layout.

Font
events invalidate even when computed font text stays the same. Horizontal and
wrapped rows additionally retain at most eight geometry tables with 4,096
allocated glyph anchors each, including glyph overflow beyond the logical CSS box.
Newly visited intervals clone bounded source before layout, validate measured
anchors, then apply the complete fragment glyph check. Wrapped slices preserve
original visual-row/tab origins, logical heights and global native offsets.

Full-paragraph measurement remains the fallback for invalid geometry, oversized
cluster slices, bidi paragraphs or reshaping differences. Cold height measurement
supplies horizontal and wrapped paint with these anchors while they
remain in the bounded cache, avoiding a second complete-row shaping pass.
Eligible unwrapped styled viewport slices reuse the paragraph preparation's
original run boundaries, guarded by immutable source/token/guide identity and
account/project/font/metric ownership. They include the preceding original run
before DOM cropping, so segmentation need not revisit the unused token prefix.
The 64 KiB slice limit and exact anchor/full-fragment checks remain. Unsupported
or uncached scopes retain the original segmentation path. Range crops preserve
inline token/run ancestors while rebuilding absolute fragment geometry, retaining
colors and italic comment styles.
Ordinary cold horizontal paint waits for the active geometry probe while native
input stays visible; explicit caret/movement probes can still reconcile source.
Equivalent syntax results preserve proven geometry for identical styled rows;
font loading advances independent font provenance even if computed font text is
unchanged.

Active-line and matching-bracket decoration tasks retain only guarded source-row
and native UTF-16 coordinates. The shared facade borrows current source and uses
the document's sparse line coordinates; queued marks are rejected after source,
selection, syntax, view or presentation ownership changes. Moving away from a
bracket skips structural preparation without copying the file. Bracket-adjacent
queries retain the existing parser/lexical fallback contracts.

Geometry publication checks the
original source/view, font and layout epochs as well as read, account, syntax and
style ownership. Retained anchors select bounded token source before HTML
generation and parsing. Partial measurement failure discards those anchors and
restores complete source before a fresh probe; Unicode offsets, token styles and
logical extents remain intact. Unwrapped rows retain equivalent styled geometry
within the same font generation without requiring a wrapped height table.

Ordinary layout reconciliation can follow equivalent syntax paint without
discarding its validated anchors; actual font invalidation always discards them.
Already-settled browser font readiness does not invalidate initial geometry;
font loading completion and failure still invalidate it.
Initial unwrapped fallback frames can paint short requested rows directly from
the immutable projection before cooperative lexical preparation finishes. The
shared facade bounds pending viewport paint to 128 rows and 64 KiB, with uniform
row geometry; full-file measurement still waits for prepared fallback tokens.
Unsupported cold viewports restore complete native text instead of moving an old
plain frame onto a different source range. Active composition retains its installed
mapping. Completed styled frames retain their existing pending-style policy.

Cold row-height preparation groups exact current paint keys, including token
boundaries, row endings and guides. After at least two measured members agree in
both width and height, the shared plan fills their remaining identical rows. Every
sample in the current batch is checked before reuse; differing dimensions retain
fresh measurement for that group. This reuses dimensions, not glyph anchors, and
keeps the same source, account, font and layout publication guards. Distinct cold
rows still require full styled measurement.

First measurement still shapes the complete row. Initial layout,
uncached full-row HTML construction and native
input costs remain performance follow-ups.

CRLF inverse mapping still selects the original CR, and surrogate offsets retain
the existing boundary behavior. Projected text, normalized textarea text and
visible-row coordinates share immutable allocations until source or folds change.
Unfolded document projections share the document's source storage, coordinate
table and lazily prepared visible-row table. The row table updates changed rows
and shifted suffixes after each edit batch; unchanged prefixes remain in place.
Retained views detach before mutation, and cold indexes do not prepare the table.
Growth reserves 256 rows of headroom rather than doubling a large table; major
deletions release excess capacity.
Folded and bounded views keep their own visible-row and coordinate tables.
LF textarea text shares the source storage too. Folded text reserves only visible
bytes and moves its assembled String into shared storage. Native normalization
and fixed-row eligibility query indexed UTF-16/display-break spans; full unfolded views use a constant-time query,
and folded views combine only visible rows. Bounded windows inspect their own
small slices. CR/LF normalization uses one pass with a source-size capacity bound. Unused projection caches release their source before edits;
retained views and composition baselines remain immutable through copy-on-write.
Projection provenance compares immutable allocations and document-version
markers, rejecting fresh documents initialized from the same source allocation.
Empty String buffer pointers likewise cannot distinguish replacements.
Retained paint visibility and source-pointer readiness are separate states; the
editor exposes `data-editor-pointer-ready` for browser checks and diagnostics.
Preparing a view does not change document identity. Short-line queries and warm
projection access are measured in [editor performance](editor-performance.md);
full String materialization and long-line query costs remain.

Both adapters retain at most eight syntax documents and 8 MiB of source, using LRU
eviction. Preparation falls back beyond 2 MiB, 50,000 lines or 100,000
metadata records; bracket and embedded-body limits still apply. Wire envelopes
are versioned and bounded before JSON parsing. These are preparation/cache limits,
not a measurement of total editor memory or final large-file performance.
Full viewport rendering and end-to-end latency/memory measurements remain roadmap
work. Prepared grammar paint retains token styles on long rows within those
source/work limits. Cancelled, oversized or unavailable analysis still uses the
cooperative plain-source renderer; no heuristic color categories are generated.

The shared document now owns fold state independently of undo history. Commands
can collapse/expand at the caret, recursively or all, and reveal a navigation target.
The projection retains logical source row numbers and maps Unicode-safe byte offsets
and directional selections between visible text and the full source. Native edits
that cross omitted text explicitly require revealing it first. Edits open affected
folds and rebase unaffected headers; stale ranges stay invalid until refreshed from
the new source. Grouped undo/redo rebases the final transaction result once.

These primitives and the folding view use the shared editor facade in both modes.

## Folding

Supported code files show fold controls beside the logical line numbers for declarations,
blocks and multiline comments/literals. Existing indicators follow line edits
while analysis is pending; their controls remain disabled until fresh ranges
arrive. The editor retains its last styled frame during same-document repaints
and replaces it with the new paint, avoiding flashes of plain text.

Replacing
a file, project, read generation or account releases that retained frame.
Click a control to collapse or expand;
the editing menu also offers cursor, recursive and all-document commands.
Ctrl/Cmd+Alt+[ folds at the cursor and Ctrl/Cmd+Alt+] unfolds; add Shift for
recursive commands. Fold state belongs to the document, including across view
and file switches during the current app lifetime.

Hidden lines remain in the source and copied selections. Syntax highlighting
retains the full file's context, and Find reveals a hidden match. Native editing,
paste, cut and composition reveal the selected source lines before the browser
changes them; disjoint folds remain collapsed. Commands and undo/redo rebase the
remaining anchors and restore the source caret through the updated projection.

Input-only browser events replay their change against the complete source.
Unchanged projected values and selections stay under the native input method's
control during composition.
Languages without a grammar use the shared language-aware lexer for bracket blocks and multiline
comments/literals, with indentation fallback when no parser is available. Adjacent
full-line comments can fold as a group.

Indentation uses the configured tab width;
blank rows do not create blocks, and multiline literal contents do not contribute
fake indentation. YAML block scalars remain opaque.

Balanced, nested `region` / `endregion` markers in language comments also fold
(for example `// #region Name` / `// #endregion` or `# region Name` /
`# endregion`). Plain text supports `#region` / `#endregion`, and C/C++ supports
`#pragma region` / `#pragma endregion`. Unmatched markers remain ordinary text.
Parser ranges take precedence over indentation; all ranges retain one
control per header and cannot cross one another. Synchronous fallback work is
limited to 2 MiB, 100,000 lines and the lexer's bracket limit. Worker rendering
and real-device input verification remain roadmap work.

## Reading and navigation

Ctrl/Cmd+G opens **Go to line/column**; enter `line` or `line:column` and press
Enter. The editing menu and cursor-status footer open the same control. Coordinates
are one-based logical source lines and Unicode character columns; a tab is one
source character. Valid coordinates beyond the file clamp to its end. Invalid
input disables Go. Navigation reveals collapsed destinations and scrolls the
source caret into view, including long horizontal lines.

Ctrl/Cmd+Shift+\ jumps to the matching bracket beside the caret. Comments and
literals remain opaque. The active line and matching brackets are highlighted,
and the footer shows line, column and selected character count. Indentation
guides use visual tab stops and complete indentation steps, independently of
source columns; blank lines continue the common surrounding indentation.
These controls also work in read-only files and share the same source-coordinate
facade in both modes.

Horizontal scrolling remains the default. **Settings → Editor defaults** offers
**Word wrap** and **Show whitespace**, saved with the existing user-scoped database
preferences. These reading options affect Edit without changing file content.
Wrapping keeps one gutter number per logical source line and measures fold-row
heights after resizing; navigation uses the rendered caret position. Whitespace
markers show spaces, tabs and line endings while retaining their original text
nodes and source offsets.

The paint adapter normalizes CRLF to match the native
textarea; the document and saved file retain their original separators.

## Find and replace

Ctrl/Cmd+F opens Find; Ctrl+H or Cmd+Alt+F opens Replace. Enter and Shift+Enter
move to the next/previous match. Search starts case-sensitive; **Match case**,
**Whole word** and **Regex** are explicit options with visible match counts.
Whole-word boundaries include Unicode letters and combining marks. Regex mode
uses [Rust regex syntax](https://docs.rs/regex/1.13.1/regex/): look-around and backreferences in the pattern are not
supported, and invalid patterns display an error. Line anchors understand LF/CRLF.

Select source text before opening Find to enable **In selection**. Search keeps
full-source context, so the selection edges do not create artificial word or line
boundaries. Replacement adjusts that scope; ordinary edits or switching files
clear it. Find reveals folded matches and works in numbered diff views.

**Replace next** changes the current match; **Replace all** changes all matches
in scope in one undo step. Enter in Replace performs next; Ctrl/Cmd+Enter performs
all. Regex replacement accepts `$1`, `${name}` and `$$` for a literal dollar;
literal replacement leaves dollar signs unchanged. Pending review, read-only
files and diff views disable replacement. Invalid patterns/scopes and size errors
leave text and history intact.

Search is bounded to 2 MiB source files, 64 KiB patterns and 100,000 matches;
replacement output is limited to 32 MiB. Empty search input has no matches.
Zero-width regex matches advance at Unicode boundaries and replacement is one
finite transaction. Large-file worker/viewport support remains roadmap work.

## File buffers

Opening another file retains unsaved text for the current app lifetime; it no
longer asks to discard the previous file. Returning restores its document history,
selection, folds and scroll position. Clean files are read again to pick up disk
changes; unsaved files keep their text. Save writes only the selected file.

Filesystem actions protect unsaved buffers even when another file is selected.
Confirmed delete/revert clears affected buffers. Loading temporarily disables
editing, and late reads cannot replace newer input or another account/project.
The file tab strip keeps opening order, shows unsaved indicators, and supports
Left/Right/Home/End navigation. Closing an unsaved file asks before discarding it;
Cancel leaves it open.

Closing the selected file chooses an adjacent tab. A close
confirmation cannot discard newer edits or files from another account/project.
The recovery facade loads saved tabs and drafts when a project opens and saves
committed changes after a short debounce. A fresh window restores the selected
file and independent drafts from user-scoped database recovery.

## Recovery contract (integration in progress)

The shared Rust recovery format and `/api/projects/{id}/editor-recovery` GET/PUT
endpoints persist tab order, selection, per-file scroll/read-only state and document
snapshots in user-scoped database settings. PUT requires the revision returned by
GET; a stale revision or changed project root returns 409. Closing every tab retains
a revision rather than deleting it, so an older window cannot resurrect its drafts.
Project deletion removes its recovery setting. Ordinary settings reads exclude
recovery bodies, and ordinary settings writes cannot bypass revision checks.

Snapshots capture committed text rather than an active IME preview. The saved
baseline, selections and collapsed folds restore with the draft; its edits become
one undoable recovery transaction. Paths, Unicode offsets, folds, scroll values,
format versions and bounds are validated in shared code. Text encoding keeps
control-heavy files within the same wire bounds as ordinary files. Limits are
64 tabs and 128 MiB of text/metadata, with each document using the editor's existing
32 MiB text limit. Failed writes preserve the preceding stored recovery.

The frontend backend facade now exposes typed recovery requests and collects
coherent active/hidden snapshots with the original saved baselines. HTTP 409 is
kept distinct from transport errors; recovery replies and save acknowledgements
are validated. Old-session REST responses cannot expire a newly active session.
Automatic loading and debounced saves are installed in the app. Saves are serialized
per project; edits arriving during a write are saved after its acknowledgement.

Failures keep the last acknowledged revision and current drafts; Retry recovery
resumes writes. A revision conflict pauses writes until Restore saved files or Keep
this window is confirmed. Late loads cannot discard files opened or edited in flight.
The disposable Spin/SQLite and Chrome check below verifies reload/new-window recovery. Native folder-permission and real-device verification remain in progress.

Recovery hydration primitives prepare all documents before publishing any state,
restore ordered tabs and active or hidden buffers, and preserve unrelated workspace
state. Guards reject a changed project, reset, pending read, document revision,
selection, draft, or composition. Old media URLs are returned to the caller for
revocation. Shared disk reconciliation preserves conflicting/missing drafts, detects
an already completed write, and refreshes clean files.

Optional disk reads distinguish
missing ancestors from permission/transport errors through the same Workspace
primitive used by rewind. Recovered files are checked before host Save is enabled,
and their baseline is checked again before a write. Missing/changed/unavailable host
files preserve drafts and show feedback with Check disk again. Existing folder access
controls restore local permissions. Review recovered file compares the current disk
file with the draft using the shared inline diff.

Cancel retains both versions;
Reload disk replaces the draft, and Save draft uses the normal save policy with a
one-use approval for the reviewed disk and draft versions. Missing files can be
recreated explicitly, including empty files. Changed editor/root/account state or
disk content rejects the action. Native folder-permission and real-device verification
remain pending.


### Live recovery verification

After building the current app, run the disposable WASI/SQLite check:

```sh
NO_COLOR=true spin build
python3 tools/check-editor-recovery.py
CHROMEDRIVER=<compatible-driver> python3 tools/check-editor-recovery.py --browser
```

The check creates its own account, database, host files and browser profile, then
removes them. It preserves the existing development services and database. The
API check covers both project modes, UTF-8/CRLF drafts and saved baselines, ordered
selected/hidden tabs, invalid/root/stale revisions, actual Spin restarts and durable
close-all tombstones. The browser check types into the built WASM editor, verifies
worker preparation for the current source, observes
its debounced database save, restarts Spin, reloads and opens a fresh window, and
checks the selected tab and draft in both modes.

Local recovery is tested without
a native folder handle; it does not establish real-device permission restoration.
Set `CHROME` if ChromeDriver needs an explicit Chrome binary.

When running the full WASM UI test suite locally, match CI’s Chrome capabilities:
include `--window-size=1280,900` in `goog:chromeOptions.args` in `webdriver.json`.
Desktop panel contracts require that viewport; individual narrow-layout tests
set their own container sizes.
CI gives the expanded frontend browser test binary 300 seconds via
`WASM_BINDGEN_TEST_TIMEOUT`; this is a whole-suite budget, independent of each
UI readiness assertion’s short deadline. Set the same environment variable locally
when running the full suite on a slower host.

Check the production worker independently with a compatible ChromeDriver:

```sh
CHROMEDRIVER=<compatible-driver> python3 tools/check-editor-worker.py
```

This serves the built assets from a disposable local HTTP server and verifies
all grammar variants, startup readiness, incremental Unicode/CRLF, a UI event
during preparation and oversized-source fallback. It checks worker inclusion in
the PWA shell; it does not establish offline editing or real-device behavior.

See [editor performance measurements](editor-performance.md) for native/WASM
text-storage comparisons and the remaining viewport/performance verification.

### Browser-engine composition verification

After building both release components, run:

```bash
CHROMEDRIVER=<compatible-driver> python3 tools/check-editor-input.py
CHROMEDRIVER=<compatible-driver> python3 tools/check-editor-input.py --windowed
```

The disposable release-app checks exercise Chromium's candidate updates, commit,
cancel, and keyboard undo/redo through the actual Rust/WASM event handlers. The
first command holds production-worker results until the first composition commits,
so syntax is pending during input. The second requires bounded native surrounding
text. Both run local and remote drafts with LF and CRLF, compare the full recovered
source with the expected bytes, and verify cancellation adds no undo step.
They also inspect the native input’s accessible name and keyboard description,
then use trusted Ctrl+M, Tab and Shift+Tab to verify focus escapes in both
directions without changing text or selection.
Composition starts/updates, beforeinput/input and keyboard events are trusted;
Chromium's CDP-generated composition-end events are untrusted. These engine checks
run in CI but do not prove physical input devices, installed Chrome/Edge PWAs,
clipboard permissions, touch selection or native local folder handles.

### Full-editor admission and large text

The shared `editor_limit` policy checks 8 MiB of source, 100,000 display lines
(LF/CRLF/standalone CR) and 1 MiB per line before interactive document metadata
is allocated. `Document::for_editor` also enforces admission on transactions,
including native input, composition previews and replicated multi-cursor edits.
The frontend facade applies this contract to existing documents. General
`Document::new` remains available for core storage experiments; its benchmarks
above these limits do not demonstrate full-editor support.

Files outside these limits open as read-only, 16 KiB text pages. Adjacent pages
preserve exact UTF-8 content, including a leading newline, and before/after
review sources remain separately accessible. Paging never replaces the full
workspace source or changes its saved state. Clean oversized recovery tabs
retain their file reference and reopen through the shared Workspace adapter;
legacy oversized drafts fail recovery admission without replacing current
buffers or deleting persisted recovery data.

These admission limits bound source and metadata growth; they are not a claim
that bounded cold wrapped paint or unique total editor memory has been validated.
Production input/scroll and process-memory baselines are recorded in
[editor performance](editor-performance.md); they still reveal stalls at admitted
file-size boundaries. Remaining work bounds cold and long-row wrapped rendering,
reduces input/source costs, and repeats responsiveness and memory validation.

### Indexed indentation guides

Guide queries borrow logical rows from the shared document index and cache an
immutable table for its source version and indentation settings. Blank runs use
the minimum indentation of their neighboring nonblank rows, including Unicode
whitespace and LF/CRLF. The standalone core helper uses the same policy. Files
above the structural byte limit share zero-guide tables across same-row-count
edits and setting changes; row-count or limit changes invalidate them. Ordinary
source edits recompute changed indexed rows and adjacent blank runs through their
nearest nonblank neighbors. Unchanged guide values retain the original table;
changed values copy retained columns around the replacement, without rescanning
unrelated source rows. Setting changes still rebuild the full table. Cached tables
remain immutable across document clones and undo.

### Cooperative plain-source paint

Worker and fallback requests share the facade's retained source snapshot for the
current file, project, account, reload and review generation. Repeated requests and
tab-width changes share that allocation, including while analysis is pending. New
snapshots copy directly from borrowed editor content, without an intermediate full
String. Source or ownership changes replace the snapshot; reset and requests
without an open file clear it. Facade-owned scopes use the shared source revision
and retained immutable allocation to validate current source without scanning bytes.
Content replacement invalidates older scopes even when the new bytes match; the
new request may reuse the same source allocation after checking those bytes.
External snapshots still require complete-byte validation, and worker transport
still serializes the requested source.

When a worker is unavailable or finishes without usable syntax paint, the editor
facade prepares fallback rows through a resumable Rust plain-source job. The same
lossless row preparation serves synchronous core callers and cooperative browser
jobs. Each batch admits at most 128 rows and 64 KiB of source, allowing one
oversized row. Rows receive one plain token rather than per-character code guesses. A first bounded batch
can finish a small file without a pending frame. Larger jobs yield browser tasks
between batches and request a frame after 4 ms of preparation or 64 batches,
whichever comes first. Missing/nonfinite/backward clocks retain the conservative
eight-batch frame schedule. This bounds rendering deferral while allowing cheap
retained rows to finish without nearly a hundred frame waits. No partial token table is
published, and pending paint borrows projected source bodies or retains an
already scoped styled frame. Full-row probes wait for cooperative fallback tokens
instead of measuring a neutral table that will be discarded. Pending background
workers retain their borrowed source preview. Both decisions use the same editor
facade, and native input stays visible during cold preparation.

Rules resolution is memoized above workspace adapters, including detected/default
rules when configuration cannot be read. Repeated ownership checks reuse that
result; source, file/project, preferences, loaded rules and indentation overrides
invalidate it through their existing reactive dependencies.

Initial geometry installs the shared source document index before syntax completes.
After native text is installed, bounded neutral unwrapped frames can publish without
an animation callback, admitting at most 128 rows and 64 KiB of source. Identical
source/font/layout frames revalidate without reinstalling DOM paint. Styled,
wrapped and oversized cold paint retain their preparation path; touch keeps the
complete native surface. Replacing a large windowed document with complete short
native text releases the old binding so its retained scroll extents cannot survive.
Browser contracts hold both frame callbacks and lexical
jobs while checking native binding and source-column/line-end clicks in both modes.

The job owns immutable source and reuses a terminal worker's source Arc when
available. CRLF normalization happens per row, preserving standalone CR and
avoiding a normalized full-file copy. Source, read, pending-edit, account, path
and tab-width changes cancel obsolete work; completed tokens are shared by
subsequent paint consumers. Completed plain-source snapshots retain raw row boundaries. Identical source with matching language and
newline normalization completes immediately and shares both row and token
tables. Shared source validates by allocation; external equal text validates bytes.
A changed source computes one UTF-8 replacement span. Whole prefix/suffix rows
reuse indexed raw boundaries without newline scans or repeated byte comparisons;
terminal rows must still terminate the new source. Intersecting rows scan their
boundaries and validate complete raw text against original/shifted offsets.
Comment delimiters do not affect fallback colors or following rows. Row
insertions/deletions and disjoint edits cannot reuse mismatching raw text. Language and newline-normalization changes
invalidate reuse. The shared worker preparation uses the same job with per-row
cancellation. Published rows use immutable shared token slices, so plain-row reuse
retains existing token strings without copying their text or token arrays. Parser
paint streams the ordered protected/embedded/semantic span boundaries without
building a complete boundary set or vector. Duplicate edges merge before paint.
Parser paint and validated worker replies use the same row representation; the painter
reads those rows without materializing a second token table. Row metadata and
handle tables still visit changed source, and source-change comparison still scans
unchanged prefix/suffix text. Shared change comparison uses byte chunks and
adjusts only differing edges to UTF-8 boundaries, retaining minimal character
spans without decoding complete unchanged text. Worker protocol v6 replies reference
unchanged token rows from the last published base ticket. Consecutive references
transfer as runs, so one edit does not serialize a reference record for every
unchanged row. The receiver checks the ticket, count, old row range, exact new row
coverage and reconstructed token budget before sharing existing allocations.
Account, document, read and tab-width changes clear the frontend base; a full-source
request without a matching worker base produces a standalone result.
Malformed references are transport failures and retain the existing safe fallback.
Replies also publish a UTF-8 replacement span when it is smaller than the full
source. The receiver validates its range, unchanged prefix and suffix, inserted
text and resulting length against the exact requested source before accepting it.
Malformed spans are rejected. Full-source requests without a matching worker
base produce standalone replies. Requests use the same span contract. If the worker
no longer has the advertised base, it asks for a full snapshot; the shared
publication facade retries once after rechecking source, account, read and ticket
ownership. A second resync response stops the transport and uses the shared
fallback. Invalid ranges remain transport failures; reconstructed byte and row
limits apply before analysis. Structural publication can transfer changed record
spans, while reconstruction and row tables still visit all rows. Validated replies retain the facade's immutable
source snapshot, and warm syntax queries use it without cloning the full buffer;
source/read/account guards still run around callbacks. Initial preparation and
changed rows still allocate token text.

Parser fallback keeps its existing 12 ms budget and
can supply ready paint before plain-row preparation is needed. Core callers that
have not installed browser preparation retain their synchronous API.

This makes terminal plain-row work cooperative; it does not make full-source
snapshot ownership, parsing, geometry or long-row initial shaping incremental. Those
remaining limits are tracked in the roadmap and performance guide.

Worker structural publication compares retained record lists and sends their changed
spans when that avoids a complete list copy. Source byte coordinates remain distinct
from record indices. A patch requires the acknowledged ticket and matching language;
small lists and language changes retain standalone data. Reconstruction checks every
list range and the aggregate expanded record budget before allocating, then applies
the existing source/coordinate/bracket validation. Structural extraction and receiver
reconstruction/validation still visit the full lists.

Pointer gestures flush pending source paint when analysis has temporarily marked
hit-testing unavailable. The current input target, source scope and glyph geometry
are revalidated before translating to source coordinates; bounded native windows
never supply whole-file pointer offsets.

Gutter digits query the active document’s source-row index, including hidden fold
rows and a trailing empty row; folds do not shrink source-number width. Native
restoration compares the input against the projection’s already-normalized value
without creating another normalized source copy.
