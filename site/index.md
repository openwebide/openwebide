---
title: Open WebIDE
description: Your code. Your models. Your workspace.
---

<div class="hero" markdown="1">

<img class="hero-logo" src="frontend/pwa/favicon.svg" alt="" width="88" height="88" />

<p class="eyebrow">Self-hosted · Open source · Local LLMs</p>

# Your code. Your models.<br>Your workspace.

An IDE in your browser, powered by the models you choose. Edit code, ask an agent
for help, review its changes, and work with Git and a terminal—all in one place.

[Get started](README.md#run-with-docker){ .btn .send }
[View on GitHub](https://github.com/openwebide/openwebide){ .btn }

<p class="release-note">Under active development toward 1.0. Try the published alpha container or a source build and share your feedback.</p>

</div>

<figure class="app-screenshot" markdown="1">

[![Open WebIDE running as an installed PWA, with Git file-change counts, a syntax-highlighted inline Git diff, and model chat](docs/assets/openwebide-pwa.png)](docs/assets/openwebide-pwa.png)

<figcaption>Open WebIDE running as an installed PWA. Review Git diffs alongside your model conversation.</figcaption>

</figure>

<div class="features" markdown="1">

<section markdown="1">

## Bring your own models

Connect to Ollama, llama.cpp, or LM Studio. Use your own inference server for
chat and coding tasks.

</section>

<section markdown="1">

## See what the agent changes

Let the agent read, edit, and run commands. Review diffs and choose the approval
mode that fits how you work.

</section>

<section markdown="1">

## Pick up where you left off

Projects, conversations, and preferences stay with your deployment. Open it from
another browser to continue working.

</section>

</div>

## Make it your workspace

Work on folders on the host or choose a folder on your browser's device.
Keep the same editor, chat, and review workflow across both.

[Explore the architecture](docs/architecture.md) ·
[Read the roadmap](docs/roadmap.md) ·
[Report a bug or ask a question](https://github.com/openwebide/openwebide/issues)

## Extend your workspace

Discover [plugins](docs/plugins.md) from Git marketplaces, with first-party
skills, memory, scheduling and monitor plugins included in fresh installs.
Plugins execute Rust compiled to WebAssembly on your execution host, including
through a paired host when your files are local. Update notifications let you
choose when to apply new releases.

Configure [MCP tools](docs/mcp.md) to connect more host services to the agent.
Use [scheduled tasks](docs/scheduled-tasks.md), [goals](docs/chat-controls.md)
and [monitors](docs/monitors.md) for ongoing work, or inspect and manage your
homelab through [host administration](docs/host-administration.md).
