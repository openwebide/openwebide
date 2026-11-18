# Reload recovery

Reloading a remote chat reconnects to its running bridge agent automatically.
Saved messages and live output merge without duplicating prompts, tool steps or
completed replies. Pending approvals remain usable. The bridge must still be
running; its active-run registry is held in memory.

If the connection drops during an approval, Chat reattaches to the same run.
An approval reappears when the bridge is still waiting for a decision. Once
you submit it, older snapshots on that connection keep the approval closed
while the bridge processes the decision.

Local agents run in the browser. Reloading interrupts that agent; **Resume**
continues from saved history with a new model turn. Finished tools keep their
recorded results. Unfinished tools are not replayed: the model receives an explicit
unknown outcome and instructions to inspect the current state before retrying.
An unknown result does not establish whether a tool ran. A fresh tool request
uses the session's approval policy. Grant folder access again if the browser asks.

Agent edits awaiting review live in the database. Reloading restores the pending
review, and Accept/Reject decisions survive subsequent reloads. If another browser
has already reviewed that revision, a stale decision refreshes the review and
cannot overwrite the other browser's decision or file contents.

## Verified scenarios

Live Chrome verification on October 5, 2026 used an isolated Spin database,
execution bridge, workspace and a deterministic provider paused before its final
reply. No production projects or model settings were used.

| Scenario | Remote | Local |
| --- | --- | --- |
| Reload after the first reply delta | Reattached; one provider request | Resume; a new provider request |
| Reload after a completed write, before the final reply | One write; pending edit restored | One write; recorded result retained on Resume |
| Accept in another window, then attempt stale Reject | Accepted content preserved | Accepted content preserved |
| Reject in another window, then attempt stale Accept | Original content preserved | Original content preserved |
| Reload again after either review decision | Decision retained | Decision retained |
| Reload at a pending tool approval | Approval reattached | Resume reports unknown outcome; does not replay the tool |

Remote checks used two independent Chrome profiles. Local checks used two windows
sharing a real origin-private File System Access directory handle in IndexedDB;
they did not exercise an OS folder picker's permission renewal. On October 5,
2026, the user confirmed that an actual OS-picked local folder continued working
after reload without prompting to pick it again. This verifies retained access;
the prompted permission-renewal path was not exercised by that check.

Shared history reconstruction and interruption detection live in
`openwebide-core`; `ProjectRuns` owns frontend recovery and selects the browser or
bridge transport. Browser regression coverage is in `streaming.rs` and `runs.rs`;
review persistence and conflicting decisions are covered by `persistence.rs` and
`pending_edits.rs`.
