# Git authentication

Git actions, `/sync` and agent Git tools use the execution bridge in both local and
remote projects. Credentials must be available on the bridge machine, rather than
in the browser or the model connection settings. Git runs without interactive
password prompts. SSH defaults to batch mode and strict host-key checking;
existing `GIT_SSH_COMMAND` or `GIT_SSH` overrides are honored.

## Native bridge

Load your key into the host agent with `ssh-add`, then check `ssh-add -l`. Verify
and trust the Git host's fingerprint on the host before using the IDE. The bridge
inherits `SSH_AUTH_SOCK` when it starts and uses the host's `~/.ssh/config` and
`known_hosts`. Restart it if the socket path changes. A service must receive the
agent socket environment too.

## Docker: opt-in agent forwarding

Private keys stay on the host. The container has an OpenSSH client; the optional
`docker-compose.ssh.yml` overlay forwards an agent socket and mounts a directory
of public configuration read-only. Prepare that directory rather than mount all
of `~/.ssh`:

```sh
mkdir -p ~/.config/openwebide/ssh
chmod 700 ~/.config/openwebide/ssh
cp ~/.ssh/known_hosts ~/.config/openwebide/ssh/
cp ~/.ssh/*.pub ~/.config/openwebide/ssh/
# Optional: copy and adapt host aliases for the container.
cp ~/.ssh/config ~/.config/openwebide/ssh/config
```

Copy only the files you have. The directory must contain only `config`,
`known_hosts` and public keys. Check host fingerprints through a trusted channel
before adding them on the host; the container rejects unknown or changed keys.
The entrypoint copies public files into `/root/.ssh` with directory mode 700 and
file mode 600. Restart/recreate the container after refreshing these files.

Adapt absolute host paths and `Include`, `IdentityAgent`, `ProxyCommand` or
platform-specific configuration before copying it. Container identities should
use `IdentityFile ~/.ssh/name.pub` with `IdentitiesOnly yes`; the matching private
key must be loaded in the forwarded agent. Host aliases still work. Do not copy
private keys, mount your whole SSH directory, or use host-only keychain helpers in
the container config.

For Docker Desktop on Mac/Linux, use its documented host agent socket:

```sh
export OPENWEBIDE_SSH_AUTH_SOCK=/run/host-services/ssh-auth.sock
export OPENWEBIDE_SSH_CONFIG_DIR="$HOME/.config/openwebide/ssh"
docker compose -f docker-compose.yml -f docker-compose.ssh.yml up -d --build
```

For native Docker on Linux, select the running host agent:

```sh
export OPENWEBIDE_SSH_AUTH_SOCK="$SSH_AUTH_SOCK"
export OPENWEBIDE_SSH_CONFIG_DIR="$HOME/.config/openwebide/ssh"
docker compose -f docker-compose.yml -f docker-compose.ssh.yml up -d --build
```

For downloaded releases, use their `docker-compose.yml` and omit `--build`. For
Tailscale deployments, use `docker-compose.https.yml` as the first file. Agent
forwarding is absent unless the overlay is selected; missing source paths fail
instead of silently becoming directories.

Confirm the forwarded identities with:

```sh
docker compose -f docker-compose.yml -f docker-compose.ssh.yml exec openwebide ssh-add -l
```

Forwarding lets container processes ask the agent to authenticate with loaded keys.
Choose which keys to load into that agent. Docker Desktop's integration is described
in [Docker's SSH agent forwarding guide](https://docs.docker.com/desktop/features/networking/networking-how-tos/#ssh-agent-forwarding).
Public-key selection is described in the [OpenSSH configuration reference](https://man.openbsd.org/ssh_config#IdentityFile).

## Podman / Quadlet

Use the commented SSH `Volume=` and `Environment=` lines in
`deploy/openwebide.container`, pointing to your user SSH agent socket. Modern systemd
provides `ssh-agent.socket` for the user session (`systemctl --user enable --now ssh-agent.socket`),
which can be referenced using the Quadlet `%t` runtime specifier (`%t/ssh-agent.socket`):

```ini
Volume=%t/ssh-agent.socket:/run/openwebide-ssh-agent.sock
Volume=%h/.config/openwebide/ssh:/run/openwebide-ssh:ro,Z
Environment=SSH_AUTH_SOCK=/run/openwebide-ssh-agent.sock
```

Prepare the same public directory as above (`~/.config/openwebide/ssh`). Rootless Podman runs as the host user;
ensure that user can access the socket. Do not relabel a shared agent socket with
`:Z`.

On SELinux hosts (such as Fedora, CentOS Stream, RHEL, or Bazzite), rootless container processes
(`container_t`) are blocked by default from connecting to host user runtime domain sockets (`user_tmp_t`),
resulting in `Error connecting to agent: Permission denied`. Add `SecurityLabelDisable=true` under `[Container]`
in `openwebide.container` to disable container label separation and allow access to the agent socket.
Drop the public-directory `:Z` on systems without SELinux.

The agent must outlive the service; if its socket changes, update the unit and restart it.
Reload with `systemctl --user daemon-reload` and restart `openwebide.service`.

## HTTPS alternative

HTTPS remotes work when the bridge's Git has a noninteractive credential helper
configured. A native bridge can use the host helper; Docker needs a helper installed
and configured inside the container. Host desktop keychain helpers are not supplied
in the image. A provider token can be supplied by that helper; keep it out of remote
URLs, prompts and tracked configuration. Never mount a private SSH key to make an
HTTPS remote work.

## Troubleshooting

- **Permission denied (publickey):** load the matching key with `ssh-add`, check
  the forwarded agent with `ssh-add -l`, and check your alias/`IdentityFile` setup.
  An unloaded, missing or inaccessible agent cannot authenticate.
- **Error connecting to agent: Permission denied:** on SELinux systems, rootless Podman
  is blocked from accessing host user runtime sockets. Add `SecurityLabelDisable=true`
  under `[Container]` in `openwebide.container` and reload systemd.
- **Host key verification failed:** verify the host fingerprint, update host
  `known_hosts`, copy it into the public directory, and recreate the container.
  A changed fingerprint requires verification too; do not disable checking.
- **Could not read Username / authentication failed over HTTPS:** configure a
  noninteractive helper/token for Git on the bridge machine.

Pull/push failures retain Git's diagnostic and include bridge credential setup
hints. A failed SSH connection does not switch transport or relax trust checks.
