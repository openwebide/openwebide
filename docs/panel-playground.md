# Panel playground

Use the [standalone playground](https://openwebide.com/tools/panel-playground/index.html) to design
plugin panels with Open WebIDE’s production Rust/WASM renderer and validator.
It runs entirely in the browser: simulated interactions never invoke a plugin,
bridge, model or other host capability. Drafts and preview preferences stay in
memory; export your work before leaving the page.

<iframe src="../tools/panel-playground/index.html" title="Interactive Open WebIDE panel playground" loading="lazy" sandbox="allow-scripts allow-same-origin allow-downloads" style="width:100%;height:900px;border:1px solid var(--border);border-radius:8px"></iframe>

Choose **All components** or an individual component example. The gallery covers
text, headings, all status tones, groups, actions, forms, lists, tables, code,
diffs, links and dividers. Form examples include text, multiline text, secret,
checkbox and select fields.

Edit the definition and document together in **Panel JSON**. Validation uses the
same API 4 panel definitions and version-1 UI schema as installed plugins. Invalid
documents stop rendering and disable exports. Switch between light/dark themes
and desktop/phone preview widths to inspect the shared component layouts.

Click a preview action or submit a form to inspect its typed **Action input**.
Secret fields are masked in that trace. Declared approval requirements are
simulated with an explicit **Approve simulation** gesture. The **Simulated
response** editor supplies the replacement view: it must be valid and return the
next revision. Editing the source cancels a pending simulation.

**Export JSON** downloads the validated UI document. **Export Rust** downloads a
`Plugin` implementation with the matching panel definition and document, using
only the public SDK. Implement the declared action handlers with SDK capabilities,
then add that definition to your plugin manifest. See [panel authoring](plugin-panels.md)
and the [plugin authoring guide](plugin-authoring.md) for the host contract,
permissions, build setup and marketplace publishing.

A visual drag-and-drop block builder is a later addition; editable JSON remains
the canonical document format and validation/export path.
