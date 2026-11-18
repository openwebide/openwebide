---
title: Open WebIDE
description: Your code. Your models. Your workspace.
---

<div class="landing-intro" markdown="1">

<div class="hero" markdown="1">

<p class="eyebrow">Self-hosted · Open source · Local LLMs</p>

# <span>Your code.</span> <span>Your models.</span> <span>Your workspace.</span>

<p class="hero-description">An IDE in your browser, powered by the models you choose. Edit code, ask an agent for help, review its changes, and work with Git and a terminal.</p>

<div class="hero-actions" markdown="1">

[Get started](README.md#install-and-run){ .btn .send }
[Browse docs](docs/index.md){ .btn }

</div>

</div>

<div>
  <a href="assets/screenshots/workspace.png">
    <figure class="app-screenshot">
      <picture>
        <source media="(max-width: 560px)" srcset="assets/screenshots/workspace-phone.png" width="390" height="844">
        <img src="assets/screenshots/workspace.png" width="1440" height="900" alt="Open WebIDE showing an inline diff for a Rust forecast project">
      </picture>
      <figcaption>View the full desktop workspace screenshot</figcaption>
    </figure>
  </a>
</div>

</div>

<div class="features" markdown="1">

<section markdown="1">

## Bring your own models

Connect to Ollama, llama.cpp, or LM Studio. [Use your own inference server](docs/first-session.md#connect-a-model)
for chat and coding tasks.

</section>

<section markdown="1">

## See what the agent changes

Let the agent read, edit, and run commands. [Review diffs](docs/first-session.md#review-changes)
and choose the approval mode that fits how you work.

</section>

<section markdown="1">

## Pick up where you left off

[Projects](docs/workspaces.md), conversations, and preferences stay with your deployment. Open it from
another browser to continue working.

</section>

</div>

<p class="release-note">Under active development toward 1.0. Try the nightly container or a source build and share your feedback.</p>

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
Choose Nightly, Preview or Stable and install compatible releases from
**Settings → Updates**, using a configured Compose or Podman/systemd updater.
