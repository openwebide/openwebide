# Editor controls

Open a file from **Files** to start editing. Switch between **Edit**, **Diff**
and **Preview** in the editor toolbar. **Diff** compares the file with the last
commit, and **Preview** appears for supported formats.

## Appearance and tabs

Settings → Editor offers all five bundled Monaspace families: Neon (the
default), Argon, Xenon, Radon, and Krypton. Texture healing and coding ligatures
are independent toggles, both enabled by default. These preferences sync through
your account and apply to Edit and diff views; fonts are available offline in the PWA. Font family and OpenType changes invalidate source geometry even when the browser cannot serialize the font shorthand.

<figure class="doc-screenshot" markdown="1">
[![The Editor tab in Settings showing the Monaspace font selector, texture healing and coding ligatures](../assets/screenshots/editor-settings.png)](../assets/screenshots/editor-settings.png)
<figcaption>Open Settings from the account menu and select Editor to change typography and editing preferences.</figcaption>
</figure>

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
that request. A newer editor command also cancels it, even if that command leaves
the text unchanged. Unavailable structure reports an error without changing the file.

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

Reviewed language-server workspace edits may create, move or delete files as
well as stage unsaved text edits. Approval waits for this window's current file
saves and flushes editor recovery before starting. File-operation intent and
recovery progress are stored in the database. An interrupted operation appears
above the editor after reload: **Check workspace recovery** retries discovery,
and **Restore interrupted changes** rolls back its acknowledged or reconciled
filesystem steps while preserving current drafts. File saves and file-tree
mutations remain paused until recovery finishes. An operation that never started
can be cancelled without folder access, without replacing later external files.
For local folders, **Grant original folder access** requests permission using
the retained original directory handle. Recovery does not substitute a different
folder automatically when the original capability is missing. **Choose original
folder** lets you select it again: affected files must match the recorded progress,
and **Grant this folder** explicitly authorizes the selected recovery target.
Granting access does not change files. Files and progress are checked again when
you confirm; a changed review requires another folder choice. This confirmation
is necessary because the browser can no longer establish folder identity after
its stored handle is lost. On the native host, rotating authentication credentials
keeps the same physical-folder identity; replacing the folder does not.
A completed operation is never silently undone by interrupted-operation recovery.
For a journal whose editor result is already committed, **Finish workspace
recovery** resumes cleanup. It preserves edits made since that decision. A cleanup
acknowledgement from this window checks the latest saved draft; a newer draft
from another window requires review before disk saves resume.
Native-object journals additionally require the paired companion and browser to
prove access to the same physical folder. A fresh proof fences older companion
sessions; lost replies cannot replay a file operation or overtake recovery.
Read/write permission is checked before and after each browser-owned native call.
Folder calls have bounded observation deadlines. A timed-out mutation requires a
fresh folder proof before recovery; the host finishes accepted work before that
proof can authorize more operations. Completed requests cancel their timers.
Regranting a folder after the irreversible decision verifies its physical identity
without requiring later file contents to match the original operation.
Git excludes these runtime names from project views and new commits or stashes.
Its destructive operations check structural collisions before replacing paths;
see [Git pane](git.md) for handling legacy tracked runtime files.

Some code actions also run a server command. Review its name and arguments with
the complete edit, then choose **Apply edits and run command**. The editor saves
approval before applying the edit and sends the command once after synchronizing
the resulting drafts. It does not automatically retry commands after disconnects.
Commands wait until the file operation and its cleanup are complete.
Pending outcomes appear above the editor. **Check command outcome** reads the
original service; **Stop pending command** fences an unstarted command or stops
its running service. Draft edits keep saving while disk writes are paused.
Review any command effects before acknowledging an uncertain result; arbitrary
effects are not automatically undone. **Discard unstarted command** keeps an
already committed edit, or cancels the command after its file operation is restored.

`python3 tools/check-workspace-transactions-client.py` verifies the production
editor, published HTML component, native LSP transport and database journal in
both modes. Build current release backend/frontend artifacts and the release
execution host with compiled bundled defaults first. The deterministic LSP peer
returns real ordered edits; the probe interrupts acknowledgement after a real
filesystem primitive, restarts the executor, loses/regrants access and preserves
newer drafts during rollback. It verifies native folder permission loss; its
browser folder uses OPFS and a picker substitute, so physical browser permission
prompts remain a separate device check. Fresh corresponding folders exercise
command approval, once-only effects, lost dispatch, reload and same-window stop,
draft persistence while a command runs, server errors and rollback before claim.

[Recorded installed Chrome PWA checks](code-intelligence/physical-folder-checks.json)
also cover real folder selection and browser permission prompts, native moves
preserving inode/hard link/mode/ACL/extended attribute/CRLF bytes, and a reviewed
command on the same physical folder. Permission loss and reload preserve drafts
and pending command recovery; Save remains fenced until manual stop and
acknowledgement. A remote move interrupted before cursor acknowledgement restores
the original native objects. A separate Local interruption after the first native
move and before editor commit survives permission revocation and reload. After
regrant, **Restore interrupted changes** reconciles the unacknowledged move and
restores the original inode and attributes. Reload retains the unsaved Unicode
draft with no pending recovery banner. These checks describe the recorded resource
checkpoint; later host changes still require integrated verification.

Production language-service reload and separate-client overlay measurements are
documented in [Language-service reload and client handoff](code-intelligence/service-measurements.md).

Unreadable, invalid or oversized configuration files produce a notice and fall
back to the remaining rules, detected indentation and defaults. Configuration
files are limited to 256 KiB. Saving `.editorconfig` refreshes the active file's
rules; switching files rediscovers rules. A pending discovery is discarded after
a project, folder, bridge or account change.

## Syntax and language behavior

<figure class="doc-screenshot" markdown="1">

[![A Rust file in Edit view with purple keywords, blue types and function names, and an italic comment.](../assets/screenshots/editor-syntax.png)](../assets/screenshots/editor-syntax.png)

<figcaption>Rust syntax highlighting in Edit view.</figcaption>

</figure>

The editor gets language definitions and source-bound analysis from installed
[host plugins](plugins.md#language-support).
Remote projects use the server execution host; local projects use the paired
host. Browsers render results and apply declared editing rules without loading
grammars or executing plugin components.

Open **Plugins** beside **Output** in the status bar to install a language package
or enable or disable it for the current project. Settings
→ Editor → Language providers stores account defaults and project-specific provider choices in the
database. Competing providers require an explicit choice; disabling a provider
or uninstalling its package restores plain editing. Unavailable analysis preserves
typing, saving and recovery; **Retry language support** requests fresh host
preparation. Re-enabling or reinstalling a provider analyzes the current draft.
The contribution contract keeps formatter preferences separate and rejects
substitution for a missing pinned formatter. Formatting and LSP services are
separate contributions: the **Code intelligence** menu uses the selected plugin's
native language server when available. Rust, JavaScript/TypeScript, C#, Python, Go and C/C++
provide language-server integrations in both project modes. Python uses Pyright
for `.py` and `.pyi` files, including project virtual-environment imports,
diagnostics, completion and reviewed rename; it does not provide formatting.
Go uses gopls for `go.mod` projects and multi-module `go.work` workspaces,
including unsaved cross-module changes, completion imports and reviewed formatting.
C and C++ share clangd, including compilation flags, unsaved header changes,
diagnostics, completion and reviewed header/source rename and formatting.
See [language support](plugins.md#language-support) for host prerequisites and
project configuration. Further providers and richer presentation remain on the
roadmap.

With a language server available, typing a name or member access opens completion
suggestions near the caret. **Ctrl+Space** requests suggestions explicitly.
Use **Up/Down** to select, **Enter** to insert, or click a suggestion. **Tab** also
inserts when Tab is assigned to indentation; in Tab navigation mode it continues
to move keyboard focus. **Escape** dismisses the popup. A completion and its
additional imports form one undoable edit and remain unsaved until you save.
Composition pauses suggestions until the input method commits its text.

Snippet completions select their first field. **Tab** and **Shift+Tab** move
between fields; linked fields update together. **Alt+Up/Down** cycles choices,
and **Escape** ends snippet editing. Tab navigation mode continues to move focus.
The **Format selection** action reviews only the selected range when supported
by the chosen formatter.

Use **Find workspace symbol…** to search a project's symbols, and filter document
symbols or navigation results by name. **Go to type definition** and **Go to
implementation** use the installed server's capabilities. **F8** and **Shift+F8**
move through current diagnostics and wrap at either end. Problems groups results
by provider, displays pending or failed analysis, and offers fixes from the
provider that reported each problem. Independent diagnostics providers have
separate enablement controls in Language providers settings.

Results and reviews remain open across unchanged editor/provider refreshes and
initial Git-status loading. Changes to effective open-buffer sources or an observed
branch/commit invalidate them while preserving drafts. Temporary Git-status loss
retains the last known revision for comparison when status becomes available again.

`python3 tools/check-language-navigation-client.py` checks navigation and
independent analysis through the production editors and public plugin API in
both modes. It installs two disposable server-only packages with deterministic
LSP peers, checks exact UTF-16 caret locations after dialogs close, resolves
lazy symbols, traverses diagnostics and stages fixes from their owning provider.
It also checks provider disablement, startup failure and recovery while preserving
editable drafts and disk files. Its local folder uses OPFS.

Pause the mouse over a symbol to see its information without moving the caret or
taking keyboard focus. The information stays open while the pointer is over the
symbol or popup; **Escape** dismisses it. Hover also works in read-only files.
The **Code intelligence** menu retains explicit symbol-information and completion
requests. Missing or unavailable language servers leave normal editing usable.

Agents use the read-only `code_intelligence` tool for diagnostics, document and
workspace symbols, definitions and references. Queries read workspace files;
editor drafts remain in a separate client. Installed packages, provider choices
and project authority are refreshed for each query. Results identify their
provider and source digest, distinguish pending diagnostics from an empty clean
result, and report omitted records when the model's output budget is reached.
Navigation input uses 1-based lines and UTF-16 columns; returned ranges use
zero-based UTF-16 coordinates. Opaque symbol and external-resource IDs must be
queried using their originating source path. External definitions are read-only
and support bounded line windows. Agent edits continue through the ordinary file
tools and their approval policy.

## Folding

Supported code files show fold controls beside the logical line numbers for declarations,
blocks and multiline comments/literals. Existing indicators follow line edits
while analysis is pending; their controls remain disabled until fresh ranges
arrive. Collapsed anchors survive edits, but pending ranges cannot hide source
until current host analysis validates them. The editor retains its last styled frame during same-document repaints
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
changes them; disjoint folds return after current host validation. Commands and undo/redo rebase the
remaining anchors and restore the source caret through the updated projection.

Input-only browser events replay their change against the complete source.
Unchanged projected values and selections stay under the native input method's
control during composition.
Language components can supply lexical folds for bracket blocks and multiline
comments/literals when no grammar is available. Adjacent full-line comments can
fold as a group. This runs on the host; unavailable providers retain core plain
editing rather than invoking a browser lexer.

Indentation uses the configured tab width;
blank rows do not create blocks, and multiline literal contents do not contribute
fake indentation. YAML block scalars remain opaque.

Balanced, nested `region` / `endregion` markers in language comments also fold
(for example `// #region Name` / `// #endregion` or `# region Name` /
`# endregion`). Plain text supports `#region` / `#endregion`, and C/C++ supports
`#pragma region` / `#pragma endregion`. Unmatched markers remain ordinary text.
Parser ranges take precedence over indentation; all ranges retain one
control per header and cannot cross one another. Synchronous fallback work is
limited to 2 MiB, 100,000 lines and the lexer's bracket limit within the host toolkit.
Real-device input and viewport performance verification remain roadmap work.

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

<figure class="doc-screenshot" markdown="1">

[![Find in file searching for average_temperature, with the first of three matches selected and search options below the field.](../assets/screenshots/editor-find.png)](../assets/screenshots/editor-find.png)

<figcaption>Find shows the match count next to the search field and highlights the current match in the source.</figcaption>

</figure>

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
finite transaction. Large-file host/viewport support remains roadmap work.

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

## Implementation reference

For development and verification, see [syntax analysis](editor-internals.md#syntax-analysis-and-editing-contexts),
[viewport rendering](editor-internals.md#preparation-and-viewport-rendering),
[document coordinates](editor-internals.md#document-coordinates-and-caches) and
[recovery](editor-internals.md#recovery-contract-integration-in-progress).

<span id="preparation-and-viewport-rendering"></span>
<span id="document-coordinates-and-caches"></span>
<span id="recovery-contract-integration-in-progress"></span>
<span id="live-recovery-verification"></span>
<span id="browser-engine-composition-verification"></span>
<span id="full-editor-admission-and-large-text"></span>
<span id="indexed-indentation-guides"></span>
<span id="cooperative-plain-source-paint"></span>
<span id="retained-parser-protocol-test-support-only"></span>
