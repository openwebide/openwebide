# Assistance model

Choose an optional **Assistance model** in the user dropdown → **Settings**.
It handles naming, summaries, suggestions, Git drafts and related-term session
search, alongside Auto approvals, compaction and delegated tasks. Your selection
belongs to your account. Leave it empty to use the primary model; unavailable or
malformed assistance results also fall back to that model.

New sessions receive automatic names from their recent activity. Names refresh
after six more user turns and at least five minutes. Renaming a session manually
keeps your name. For scheduled tasks and memories, leave the title empty to name
it automatically; automatic titles refresh when their contents change. Agent
updates preserve titles you assigned yourself.

Completed chats show a collapsible **Where you left off** recap, a short result
summary marked with a **↪** recap indicator and suggested next prompts as inline
composer hints. Summaries describe the topic or concrete result directly,
mentioning failures or blockers only when relevant. Follow-up suggestions are
prompts you can send to the assistant, rather than questions addressed to you.
A hint completes matching
text as you type. Press **Right Arrow** at the end of the input to accept
it; acceptance fills the composer without submitting. Cursor movement, text
selection and modified arrow keys keep their normal behavior. Follow-up choices
also appear as compact outlined prompt buttons in the chat history; clicking one
sends it immediately. Activity
labels describe current tools and approval waits directly from their execution
state.

When you write a prompt, the app may suggest files, saved memories and previous
chats from the current project. Projectless chats can suggest other projectless
chats. Context suggestions appear as clickable items in the chat history.
Select one to attach it; file contents are resolved through the
normal file-reference workflow when you send. Unknown identifiers and stale
suggestions are discarded.

The Git pane's draft menu can produce an editable commit message, branch name or
PR description from the current diff, new text files and repository conventions.
Copy or edit the result yourself. Changed repository inputs invalidate a pending
draft, and generation preserves edits you make while it runs.

Session search shows literal matches immediately, then adds related-term matches
with an explanation of where each term matched. If generation fails, the literal
matches remain available. Scheduled-task results and completion notifications
summarize the actual final response and tool outcomes, with an excerpt fallback.

Optional chat assistance waits for idle time and uses one queue. Generation is
bounded, tool-free and cached in your account's database. Late results cannot
update another account, project or session. The same workflows serve local and
remote projects; Git drafts and project memory require a project.
