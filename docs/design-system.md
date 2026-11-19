# Frontend design system

## Foundations

Use `frontend/src/components/ui.rs` and the tokens in `frontend/styles.css` when
composing controls. Colors, spacing, typography, radii, dialog sizes and control
height come from shared tokens. Existing shared CSS classes remain supported as
components migrate; avoid private control styles in individual dialogs or panes.

Icons come from the Rust `lepticons` Lucide library through `ui::Icon`. Category
features scope the dependency to app controls. `Icon` renders decorative 20px SVGs
in the current theme color; the parent control supplies meaning. Use SVG action
icons instead of emoji or font-dependent glyphs.

## Dialogs, buttons and menus

`Modal` owns focus, dismissal, header and size. Compose its scrolling content with
`DialogBody`, `FormSection`, `FormField`, `CheckboxField` and `FormNotice`. Put final
actions in `DialogActions`, outside the scrolling body. Use sentence case for
sections and keep helper text beside its field. Group controls with
`FormField group=true` rather than putting several controls inside a label.

Use standard `Button` variants and `InlineActions`. `IconButton` needs an accessible
label; top-bar actions retain desktop text. Tooltips appear after 200ms on hover
and immediately on keyboard focus. They escape clipping and dismiss on Escape,
pointer activation, scrolling or cleanup.

`Dropdown` owns menu surfaces, viewport fitting, focus, keyboard navigation and
dismissal. `DropdownSelect` builds value choices on it. Recent projects, models,
approval modes, branches, wizard choices and settings selectors share these
primitives. Use `SegmentedControl` for compact mutually exclusive views or modes.

`ActionMenu` groups secondary actions behind a right-aligned Lucide ellipsis and
reuses `Dropdown`. Keep it inline with the label or view switcher; truncate long
labels instead of wrapping the menu onto another row. Items use explicit labels
and existing callbacks. Dialog handlers must outlive the temporary menu surface.
Keep Send, Stop, Save and Cancel visible.

## Panels and layout

`ToolPanel` owns headings, ordering/minimize actions, dock widths and full-height
resize boundaries. Feature panes provide content. Headings always show Minimize;
only panels with additional actions show an overflow menu. `PanelToolbar` groups
contextual information and actions below that heading. Panes do not repeat the
panel or selected-view title.

Each shared vertical boundary has one trailing grip inside the panel edge,
independent of content wrappers. Resizing changes the fixed dock beside the
flexible Editor or Chat so the seam follows the pointer after reordering. Only
the dragged grip highlights. Adjacent tool windows share resize and keyboard behavior.

`LayoutState` owns limits and fitting. `LayoutActions` saves preferences and widths
through user-scoped database settings, serializing writes and guarding account
generations. Browser events belong to `PanelResizer`; feature operations use the
Workspace and ProjectGit facades.

Phone tool sheets use the available width and retain desktop preferences.
Collapsing panels and switching file views preserves mounted state. Server rows
preserve the server name before provider metadata when space is limited.

## Files, search and Git

`FilesPanel` composes Explorer/Changes selection inside `SearchPane`, which switches
between mounted file views and results. `PanelSearchRow` gives Files, Sessions and
Find in file the same input and inline actions. Explorer and Changes share row
spacing, icons and selection treatment. Creation actions appear in their shared
toolbar only when Explorer is selected.

`BranchPicker` uses the shared dropdown for branch selection and New branch in
Changes and the footer. Keep it mounted across status refreshes; update badges
separately. Discover branches on branch/scope changes or explicit menu opening.
`GitActions` owns discovery, checkout and prompt scope through ProjectGit adapters,
guarding late results by project, account and host revision.

## Editor layout

Gutters fit the file's largest line number and stay pinned during horizontal
scrolling. Inline diffs use old/new numbers; Split numbers each side. Split panes
share horizontal/vertical scroll positions and content width.

Edit's text viewport sits beside its gutter. Syntax paint is clipped above the
horizontal scrollbar so it cannot cover themed tracks or thumbs.

## Chat and output

Reasoning, run context and tools use `DisclosurePanel`: left-aligned headers,
down/up carets and retained collapsed content. Tool calls share counts in first-use
order; approvals force their group open. Active reasoning adds timing, token
estimates and a spinner. Transcript entries retain their height when history
exceeds the viewport; the stream scrolls around those headers.

The composer uses a shared browser sizing primitive. It grows to at most 80% of
its pane while reserving transcript/chrome space, and shrinks when drafts change
programmatically. Whole-pane drops and paste/picker input call `Composer::import`,
preserving validation and project/session/account guards.

Chat uses compact labeled icon controls beside the composer. Enter sends or queues,
Ctrl/⌘+Enter steers an active run, and Escape stops it. Attach images lives in the
Chat menu; paste/drop use the same import path. Attachments use a compact
composer indicator with expandable previews. Inline prompt hints and clickable
history suggestions keep the area between the status line and input clear. Chat
actions restore composer focus after menus settle, respecting open dialogs. Telemetry stays on one line, hiding
secondary fields in narrow panes. The context gauge opens the context details;
phones show the percentage while preserving model and approval controls.

Terminal is a full-width bottom dock controlled from the status bar and available
only with a project. Its top-edge separator resizes the database-backed height
without consuming editor width. Its Output header combines connection and actions;
command input appears from its terminal icon or **New shell**. **Copy output**
copies retained rendered lines. On phones, Output overlays the current pane as a
sheet, and a full-width bottom bar provides equal icon-and-label destinations.
Account actions live in the username dropdown.

Chat details use the shared modal and context-breakdown styles. Run-context
snapshots open from the prompt action menu; generation speed opens measured
statistics with token and recent-rate bars. Chat dialogs mount outside the pane's
contained layout, preserve focus, and close when their chat or account changes.

Settings, Plugins and keyboard shortcuts use the shared `DialogTabs` component
with one tab stop, arrow-key navigation with wrapping, Home/End, named panels, and automatic
activation. The header, tab list and footer stay visible while content scrolls.
Settings groups General, Editor, Models and Host. Switching tabs or collapsing
a Settings section retains field drafts.
Model defaults save on selection, with saving status and a retry action after errors.
Autosaves stay queued when the dialog closes and ignore stale account responses.
All pane headers use the same minus control to collapse their retained content.
