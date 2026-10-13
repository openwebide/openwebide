# Running Open WebIDE as a Podman quadlet

Quadlet lets systemd manage Open WebIDE as a Linux service, with SQLite and
project files persisted on the host. Use a recent Podman with Quadlet `.image`
support and systemd. See [Podman's Quadlet reference](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html).
This deployment is not available on macOS/Windows.

Host administration is included in the image. Configure its SSH connection in
**Settings → Host** with your SSH username to manage the actual Linux host.
The host address is automatic. Use the SSH credentials below; no privileged
container or second bridge is required. See
[host administration](host-administration.md).

For Git over SSH, see [agent forwarding and public SSH configuration](git-ssh.md#podman-quadlet).

## Install a published release

Download `openwebide.image` and
`openwebide.container` from [GitHub Releases](https://github.com/openwebide/openwebide/releases).
The image unit tracks its release channel (`stable` for stable releases, or
`nightly`/`preview` for prereleases); no checkout or build is needed.
Repository templates in `deploy/` default to `stable`.

```sh
mkdir -p ~/.config/containers/systemd
mkdir -p ~/.local/state/openwebide/data ~/source
cp openwebide.image openwebide.container ~/.config/containers/systemd/
systemctl --user daemon-reload
systemctl --user start openwebide.service
systemctl --user enable --now podman-auto-update.timer
```

The `.container` uses `Image=openwebide.image`, which makes Quadlet generate a
dependency on `openwebide-image.service`. The generated app service is
`openwebide.service`, not `openwebide.container`. `[Install] WantedBy=default.target`
starts it when the user manager starts; generated units are not enabled with
`systemctl enable`. To keep the user service running without an interactive login,
configure user lingering (`loginctl enable-linger "$USER"`).

Before starting, edit the workspace mount if your projects are outside `~/source`.
The supplied units use:

```ini
# openwebide.image
[Image]
Image=ghcr.io/openwebide/openwebide:stable

[Service]
TimeoutStartSec=900
```

```ini
# openwebide.container
[Container]
Image=openwebide.image
Pull=newer
AutoUpdate=registry
ContainerName=openwebide
PublishPort=127.0.0.1:8080:3000
PublishPort=127.0.0.1:3001:3001
Volume=%h/.local/state/openwebide/data:/app/.spin:Z
Volume=%h/source:/workspace:Z

[Service]
Restart=always
TimeoutStartSec=900

[Install]
WantedBy=default.target
```

The `:Z` suffix relabels bind mounts for SELinux; drop it on systems without
SELinux, and choose the workspace scope deliberately. The bundled entrypoint
keeps Spin's host-write mount flags and runs the bridge against `/workspace`.
Its persistent secret lives in the mounted data directory.

Open <http://localhost:8080/> and register the first account.

### Networking and port configuration

- **IPv6 and `localhost` resolution**: On systems where `localhost` resolves to `::1` (IPv6), binding only `127.0.0.1` can result in connection failures when accessing `http://localhost:8080/`. To support both IPv4 and IPv6 loopbacks, specify both `PublishPort=127.0.0.1:8080:3000` and `PublishPort=[::1]:8080:3000` (and port 3001), or bind without an IP prefix (`PublishPort=8080:3000` and `PublishPort=3001:3001`) to listen on all interfaces.
- **Port conflicts**: If port 8080 is already in use by another service on the host (such as a local `llama.cpp` or Ollama instance), map to an alternate host port such as `PublishPort=8081:3000`.
- **Connecting to host model servers**: Because Open WebIDE runs in its own network namespace, `localhost` inside the container refers to the container itself. In **Servers**, point to model servers running on the Linux host using `http://host.containers.internal:<port>/v1`.
- **Tailscale and LAN access**: When accessing Open WebIDE over a LAN IP or Tailscale domain, configure the bridge allowed hostnames and origins in `openwebide.container`:
  ```ini
  Environment=OPENWEBIDE_BRIDGE_ALLOWED_HOSTS=<device>.<tailnet>.ts.net
  Environment=OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS=https://<device>.<tailnet>.ts.net
  ```
  For private HTTPS via Tailscale Serve running on the host, see the [Tailscale guide](tailscale.md#host-tailscale-serve-with-container-deployments-podman-quadlet-or-docker).
- **Health check**: `/api/health` requires authentication; an unsigned 401 is expected.

### Host administration and SSH setup

Host administration and Git over SSH use the host user's SSH agent forwarded into the container. Because automated commands run non-interactively with `-oBatchMode=yes`, key authentication is required:

1. **Enable host SSH server**:
   Ensure `sshd` is active on the host (`systemctl enable --now sshd`).
2. **Authorize an SSH key on the host**:
   Ensure your host user account has an SSH key pair authorized in `~/.ssh/authorized_keys`:
   ```sh
   mkdir -p ~/.ssh && chmod 700 ~/.ssh
   ssh-keygen -t ed25519 -N "" -f ~/.ssh/id_ed25519
   cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys
   chmod 600 ~/.ssh/authorized_keys
   ```
3. **Enable user SSH agent and load key**:
   Start the user systemd agent socket and add your key:
   ```sh
   systemctl --user enable --now ssh-agent.socket
   export SSH_AUTH_SOCK="/run/user/$UID/ssh-agent.socket"
   ssh-add ~/.ssh/id_ed25519
   ```
4. **Prepare public SSH configuration**:
   Prepare `~/.config/openwebide/ssh` with public keys and host fingerprints:
   ```sh
   mkdir -p ~/.config/openwebide/ssh && chmod 700 ~/.config/openwebide/ssh
   cp ~/.ssh/id_ed25519.pub ~/.config/openwebide/ssh/
   ssh-keyscan -p 22 host.containers.internal >> ~/.config/openwebide/ssh/known_hosts
   chmod 600 ~/.config/openwebide/ssh/known_hosts
   ```
5. **Configure `openwebide.container`**:
   Add the agent socket, public config volume, and automatic SSH host under `[Container]`:
   ```ini
   Environment=OPENWEBIDE_HOST_SSH_HOST=host.containers.internal
   Volume=%t/ssh-agent.socket:/run/openwebide-ssh-agent.sock
   Volume=%h/.config/openwebide/ssh:/run/openwebide-ssh:ro,Z
   Environment=SSH_AUTH_SOCK=/run/openwebide-ssh-agent.sock
   ```
   **SELinux hosts (Fedora, Bazzite, RHEL)**: Rootless container processes (`container_t`) are blocked by default from connecting to host user runtime domain sockets (`user_tmp_t`), resulting in `Permission denied` errors. Add `SecurityLabelDisable=true` under `[Container]` to permit socket access:
   ```ini
   SecurityLabelDisable=true
   ```
6. **Reload and restart**:
   ```sh
   systemctl --user daemon-reload
   systemctl --user restart openwebide.service
   ```
7. **Configure in Settings → Host**:
   - Check **Enable host administration**.
   - Set **SSH username** to your host user account.
   - The host operating system is detected automatically.
   - Under **Advanced connection settings → Pinned public host keys**, paste the verified host public key for `host.containers.internal` (or leave blank if included in the mounted `known_hosts`).
   - Select **Save and test connection**.

```sh
systemctl --user status openwebide.service
journalctl --user -u openwebide.service -f
```

For a system service, install units in `/etc/containers/systemd/`, replace `%h`
volumes with absolute paths, use `WantedBy=multi-user.target`, and omit `--user`.

## Build from source

Until a public image exists, or to test a checkout, build the Dockerfile with
Podman:

```sh
podman build -t openwebide:local .
```

Use the same container unit but change its image line to `Image=openwebide:local`.
Remove `Pull=newer` and `AutoUpdate=registry` for this locally built image.
The image unit is unnecessary for this path. Reload systemd and start the app
service as above; rebuild and restart the service after source changes.

## Upgrade

`Pull=newer` checks for a newer image when the container starts; `AutoUpdate=registry`
lets `podman-auto-update.timer` check the registry periodically and restart the
service when the tracked image changes. The supplied timer runs daily by default.
Check upcoming runs with `systemctl --user list-timers podman-auto-update.timer`,
or preview available updates with `podman auto-update --dry-run`.
See [Podman's automatic-update documentation](https://docs.podman.io/en/latest/markdown/podman-auto-update.1.html).

Keep `stable` to receive new releases automatically. A `v1.0.0` tag or digest stays
on that version; for manual upgrades, pin the image and remove `AutoUpdate=registry`.
Choose `nightly` or `preview` to follow [prerelease updates](releases.md#nightly-preview-and-stable-channels).
Automatic updates restart the app and may interrupt active runs. Keep regular data
backups using the [release guide](releases.md#upgrade-and-restore).

Stop the app and back up `~/.local/state/openwebide/data/` first. Set the target
version in `openwebide.image`, then:

```sh
systemctl --user daemon-reload
systemctl --user restart openwebide-image.service
systemctl --user start openwebide.service
```

Keep the data/workspace paths unchanged. Read [upgrade and restore guidance](releases.md#upgrade-and-restore)
before switching versions; a database schema upgrade requires restoring the
pre-upgrade backup to roll back safely.
