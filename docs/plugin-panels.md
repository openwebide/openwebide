# Plugin panels

API 4 adds host-executed, declarative panels. An enabled plugin's panels appear
beside the built-in workspace navigation entries. Open a panel by selecting its
entry; minimize it to return to your work. Its menu moves it left or right.
Visibility and placement are saved with your account's workspace layout.
On a phone, a panel occupies the workspace sheet. Selecting Chat returns to the
conversation; disabling the open plugin also returns to Chat.

Panels use the execution host selected for the workspace. Their Rust executes on
the server bridge or paired host, including when the client is a phone. Browsers
render validated documents through the same components and themes as the app.
They never execute plugin browser scripts, HTML or plugin WASM.

## Loading and actions

A panel shows progress while loading and an error if its host or handler is
unavailable. The refresh icon retries a view. After a failed action, refreshing
opens a fresh instance and reads the current state; it does not repeat the action.
Core workspace navigation remains available when a plugin fails.

Plugins can request periodic refreshes. Refresh pauses while the browser tab is
hidden, while you edit a form, or while an approval dialog is open. Buttons are unavailable during a request. Actions
requiring approval show the plugin, action and submitted inputs before execution.
Review destinations and contents before approving a publishing action.

Requests belong to a specific account, project, installed plugin version, panel
instance and view revision. The database records execution claims and canonical
views. Claims store an input fingerprint and revision rather than submitted form
values, including while execution is pending. The eight most recent view revisions
retain receipts; older requests fail canonical-revision validation and cannot run
again after their receipts are discarded. A repeated recent request returns its
receipt or pending state instead of starting another execution. Closing a panel
clears its documents and claims while retaining a retired instance identity.
Instances idle for an hour are retired when another panel request begins, so
abandoned browser windows cannot permanently exhaust the 64 active-instance slots.
Reopen or refresh an expired panel to obtain a new view. Interrupted actions are not automatically replayed. Switching
projects/accounts, closing a panel, disabling a plugin or updating its code rejects
obsolete results. Configuration stored through SDK records survives panel closure;
form drafts belong to their current view.

## Authoring a panel

Declare `compatibility.pluginApi: 4`, a Rust executable and
`contributions.panels`. A package may also contribute tools, skills and events.
Each definition includes `id`, `title`, `icon` and a `view` handler. Declare action
IDs, their handlers, titles and `requires_approval`. Optional `refresh_seconds`
must be between 5 and 3,600. The installed component's `Plugin::panels()` exports
must exactly match its manifest definitions.

Implement `Plugin::panel(input)` using the public Rust SDK. For a view,
`input.action` is absent and `input.handler` names the declared view handler.
For an action, both identify the declared action. Return a `PanelDocument` with
UI `schema_version: 1` and `revision: input.revision + 1`. Manifest compatibility,
SDK compatibility and the document schema version are separate contracts.

Supported blocks are plain text, headings, status notices, groups, actions, forms,
lists, tables, code, diffs, HTTP(S) links and dividers. Forms support text,
multiline text, password fields, checkboxes and selects. Password fields must have
an empty initial value; plugins cannot return stored credentials in those fields. Use typed SDK values; arbitrary markup,
styles, client expressions and unknown properties are rejected.

Button `values` are immutable routing data, such as a pull-request number. A form
adds only its declared, typed fields. The host validates submissions against the
saved document, including required fields, options, lengths, disabled buttons,
state and revision. Do not put credentials in documents, button values or opaque
view state. Retrieve private configuration through the SDK's scoped records.

View handlers are read-only: HTTP GET/HEAD with empty bodies and supported read
operations on host data. Mutations belong to declared actions. Actions use the
same capability grants as other SDK contributions; the UI does not add privileges.
A panel namespace includes its repository/package identity, so two marketplaces
cannot accidentally share layout or state by choosing the same display name.

## Bounds

Documents are limited to 256 KiB, 256 blocks and six nested layout levels. A
package can declare 16 panels with 32 actions each. Forms have at most 16 fields;
actions blocks have eight buttons, lists have 100 items, and tables have eight
columns and 100 rows. Inputs and opaque state each have a 32 KiB limit. Text/code
and individual controls have additional limits enforced by the SDK schema and
host validator. Keep requests short and handle host failures explicitly.

See [plugin authoring](plugin-authoring.md) and the marketplace repository's
[panel schema](https://github.com/openwebide/plugins/blob/main/schemas/panel.schema.json).
A component gallery and interactive authoring playground remain on the roadmap;
they will reuse this renderer and schema.
