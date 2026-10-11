# Host administration

Use project-less chat to inspect and manage your homelab machine over SSH. The
SSH client and host administration are included in the standard image and native
bridge. There is no extra feature flag or host-specific Open WebIDE executable.
Until you configure a connection, host tools are unavailable.

## Connect the machine

1. Enable SSH on the target machine and authorize the key for the account you
   want to use. The target can be Linux, macOS or Windows with an SSH server.
2. Make that key available to the server bridge. Native bridges inherit the
   current SSH agent and configuration. Containers use the
   [SSH agent forwarding overlay](git-ssh.md#docker-opt-in-agent-forwarding) or the commented agent mounts
   in the [Quadlet template](podman-quadlet.md). Keys remain in the agent;
   the app does not upload or store private keys. SSH login uses key authentication.
3. In **Settings → Host**, enter an SSH alias or `user@hostname`, port and OS.
   Enable host administration (selected by default). Use an address reachable
   **from the bridge**, such as the host's LAN address. Container `localhost`
   refers to the container, not your physical machine. Aliases must exist in the
   bridge's public SSH configuration.
4. Paste public `known_hosts` entries verified through another trusted channel,
   or use the bridge's existing verified host-key file. Strict host-key checking
   stays enabled. Save the connection, then select **Test connection**.

This is shared, administrator-owned server configuration, stored in SQLite.
Changes invalidate prepared operations; connection settings cannot change during
active maintenance. Docker and Quadlet examples include the capability by
default, but need the SSH credentials and connection above before use.

## Inspect and manage

Open a project-less session and select **Host** in the chat statusline to open
a compact flyout. Inventory and operations refresh while it is open; closing it
restores the space to chat. Use it to view the machine identity,
OS/version, CPU and RAM, services, processes, containers, ports, storage and
configuration. Filter resources and select **Ask about this** to add the observed
resource to your next message. Missing utilities appear as inventory notes;
inspection still returns the collectors that succeeded. Container inspection uses
Podman when installed, otherwise Docker, and includes published ports and mount
sources. On macOS and Windows, the runtime can use a separate VM; runtime-reported
paths are labeled accordingly. See the [Podman inspection reference](https://docs.podman.io/en/latest/markdown/podman-container-inspect.1.html)
and [Docker inspection reference](https://docs.docker.com/reference/cli/docker/inspect/).

The agent receives the observed host and shell context when its run starts.
Unix commands use POSIX shell syntax; invoke Bash explicitly for Bash scripts.
Windows commands use PowerShell. The selected host is independent of your
browser computer, model server and project execution environment.

Try requests such as:

- “Set up a user Podman Quadlet for Plex, using /srv/media, and verify it starts.”
- “Why is this service failing? Inspect its configuration and recent logs.”
- “Prepare an OS update and include post-update checks.”

The agent inspects first, prepares an immutable plan containing exact commands,
working directories, timeouts, elevation and verification steps, then requests
explicit approval. Host changes always require approval, including in YOLO mode.
Preparation does not execute the plan. A running operation means it has started;
review the operation's retained results to see whether commands and checks passed.

## Interactive prompts and elevation

Interactive commands get an SSH terminal. Expand their host operation to see
live output and enter a **Private terminal reply** for sudo passwords or other
prompts. Replies go directly to that terminal rather than through the model or
chat history. Exact echoed replies are redacted from retained output. You can
send an interrupt from the same operation. Noninteractive commands use `sudo -n`
when elevation is requested, and fail if authentication needs a prompt.

Linux and macOS can elevate individual approved commands with sudo. The IDE
bridge keeps its normal privileges. Windows uses the SSH account's existing
privileges; interactive desktop UAC elevation is not supported over this path.

## Disconnects and reboot

Operations run independently of the requesting chat. Closing a tab or stopping
chat leaves maintenance running; reconnect to inspect the same operation and
reply to an active prompt. Commands, results and status live in the database;
private prompt replies do not. Only one operation may maintain a target at once.

A reboot plan must include verification commands and identify its final step as
an intentional reboot. After the host returns with a new boot identity, the
resident bridge runs verification without replaying mutation commands. The
bridge also retries recovery after SSH/backend outages. If the bridge itself
restarts during ordinary maintenance, the operation becomes interrupted and
needs inspection; it never automatically repeats its changes. An expected reboot
that is not observed within an hour becomes interrupted.

Keep the container or native bridge configured to restart at boot. The supplied
Compose restart policy and Quadlet user service already provide their normal
restart behavior. Linux user services may need lingering to start before login.
Project sessions and paired browser companion bridges cannot acquire these host
administration tools. Only the authenticated server bridge uses this connection.
