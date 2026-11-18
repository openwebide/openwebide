# File and folder menus

Right-click a file or folder, hold it for half a second on touch devices, or focus
it and press Shift+F10/the context-menu key. The inline **…** button opens the same
menu. Action rows use the same icons in dropdown and right-click menus. Escape
or an outside click dismisses it; arrow keys navigate the menu.

<figure class="doc-screenshot" markdown="1">
[![The forecast.rs file menu showing Rename, Move, Copy path, and Git actions including Stage and View file history](../assets/screenshots/file-actions.png)](../assets/screenshots/file-actions.png)
<figcaption>The file menu groups actions for the selected row. Git actions depend on that file's current status.</figcaption>
</figure>

The Files **…** menu contains New file/folder, Refresh files, and Include hidden
files and folders. The hidden-file preference syncs through your account. Tree
rows are compact outside phone mode; phone mode uses larger touch targets.

Colored file/folder icons describe Git changes; tracked text changes show added
and removed line counts aligned at the right edge before the **…** button.
Hover or use a screen reader for the status description.
Binary and untracked files have status icons without invented line counts.

Rename, move, copy the project-relative path, or delete an entry. New file and
New folder are available on folders and the Files toolbar. Editor tab menus
also offer Reveal in Files, which expands and focuses the corresponding tree row.
Move
accepts a destination relative to the project; its parent folder must exist.
Delete requires confirmation. Save or discard affected unsaved editor changes
before rename, move, stage or ignore; resolve pending edit reviews first.

Git actions follow the file/folder's index and working-tree status. Ignore appends
an anchored literal rule to `.gitignore`, preserving existing rules and newline
style. Revert restores HEAD in both the index and working tree, including a
rename's original paths, after confirmation. Untracked files remain; a conflicting
untracked replacement blocks Revert. Unstage also works before the first commit.
Git requires an available execution bridge and, for local projects, a matching
host folder. Filesystem and chat actions remain available without Git.

Explain, Summarize and Review changes send a path-aware prompt immediately through
the normal chat workflow. Explain asks how the code works; Summarize asks for a
concise overview; Review looks for bugs and regressions in the changes. An existing
composer draft and attached images remain unsent. Git Changes rows share the Git
and chat actions, without filesystem creation/rename/delete actions. Review uses
known changes even while menu-specific Git discovery is pending.

Both workspace modes use the same mutation policy. Moves copy and verify before
removing the original; they are not atomic. A failed operation reports whether
an incomplete destination or both locations remain. Moves are limited to 10 MiB
per file, 32 MiB total and 10,000 entries, and reject symbolic-link paths and
Git/runtime internals. Larger moves should use the host filesystem tools.

Existing **…** menus on sessions, servers, system prompts, chat messages and panel
headers also support right-click, long press and the keyboard context-menu keys.
The browser context menu is suppressed throughout the app, including blank areas
and inputs; only elements with an app context menu show one.

The Files header keeps **Tree** and **Changes** available during search. Its
search icon reveals and focuses project search; Escape cancels search and returns
focus to the icon. Switching views retains the mounted file controls.

The Files menu groups creation, tree and panel actions. **Expand all** discovers nested visible folders and expands filename groups in both workspace modes; **Collapse all** also cancels pending expansion. Large trees stop at 512 folders or 64 levels and report the limit.

Sibling filename variants nest beneath their closest existing base file, such as
`docker-compose.ssh.yml` under `docker-compose.yml` and `app.ts.map` under `app.ts`.
The caret expands the group; clicking the filename still opens that file. Paths
and file operations stay unchanged, and variants appear as ordinary rows when
their base file is missing. Arrow keys and Reveal in Files navigate nested groups.

Project and file tab menus include Close, Close others, Close all to left/right,
and Move left/right. Middle-click closes the target tab. Closing a file uses the
same unsaved-change prompts as its close button; pointer selection preserves the
tab strip's horizontal scroll, and keyboard selection can scroll its target into view.
