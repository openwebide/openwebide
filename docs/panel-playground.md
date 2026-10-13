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

Start with **New panel** or a gallery example. Click a palette item to add it;
when a group is selected, new blocks go into that group. Drag palette handles
or existing block handles onto the insertion lines to add, reorder or nest blocks.
Mouse, touch and pen use the same editing operations. Dragging near the page edges
scrolls the layout. Invalid cycles, excessive nesting and schema limits leave the
document unchanged.

Select a block to edit its properties. Controls cover text, notices, links, code,
diffs, form field kinds, action buttons, list items and table columns/cells.
**Apply properties** validates the complete document before updating the preview;
invalid property drafts leave the last valid document intact. Table column moves
keep their cells aligned. The top-level settings icon edits the panel definition
and declared actions. Renaming an existing action also updates its button bindings;
referenced actions cannot be removed until their buttons are changed or removed.

The block's **…** menu provides move-up/down, duplicate and delete actions.
Duplicated forms receive unique field IDs. For keyboard placement, select a block,
choose **Destination group** and **Position**, then use **Move block**. Undo/redo
includes visual changes, source edits and simulated results, with bounded history
kept only in the current page's memory.

Open **Source JSON** to edit the definition and document together in **Panel JSON**.
Visual editing, source editing, simulations and exports share one JSON document.
Changing the source cancels stale gestures and property drafts. Validation uses the
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

The visual editor uses the production schema; it introduces no separate UI format
or executable plugin code in the browser.
