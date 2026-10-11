# Hosting and administration

The container bundles the Spin backend and native execution bridge. Mount your
project directory and persist the app's database before using it for ongoing work.

- [Releases, configuration and backups](releases.md): installation files,
  environment settings, release channels, upgrades and restoration.
- [Podman services](podman-quadlet.md): rootless systemd services and automatic
  registry updates.
- [HTTPS with Tailscale](tailscale.md): private access across devices without
  exposing the app publicly.
- [Host administration](host-administration.md): configure SSH, inspect the physical
  homelab machine and approve maintenance from project-less chat.
- [Git authentication](git-ssh.md): SSH agents, container forwarding, host keys
  and HTTPS credential helpers.
- [Execution bridge](execution-bridge.md): native execution, authentication,
  browser pairing and connection configuration.

For a first installation, begin with [the quick start](../README.md#run-with-docker).
For an existing installation using the old deployment name, read the
[volume migration notes](releases.md#existing-installations-with-the-old-project-name)
before changing Compose configuration.

See [MCP tools](mcp.md) to configure host processes that extend the agent’s tools
in remote projects and through paired local execution hosts.
