# Running Open WebIDE as a Podman quadlet

Quadlet lets systemd manage Open WebIDE as a Linux service, with SQLite and
project files persisted on the host. Use a recent Podman with Quadlet `.image`
support and systemd. See [Podman's Quadlet reference](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html).
This deployment is not available on macOS/Windows.

Host administration is included in the image. Configure its SSH connection in
**Settings → Host** to manage the actual Linux host, using the SSH credentials
below; no privileged container or second bridge is required. See
[host administration](host-administration.md).

For Git over SSH, see [agent forwarding and public SSH configuration](git-ssh.md#podman-quadlet).

## Install a published release

Download `openwebide.image` and
`openwebide.container` from [GitHub Releases](https://github.com/openwebide/openwebide/releases).
The image unit tracks its release channel (`latest` for stable releases, or
`alpha`/`beta`/`rc` for prereleases); no checkout or build is needed.
Repository templates in `deploy/` default to `latest`.

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
Image=ghcr.io/openwebide/openwebide:latest

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

Open <http://localhost:8080/> and register the first account. Both ports bind to
loopback; use [private HTTPS via Tailscale](tailscale.md) for other devices.
If explicitly exposing ports on the LAN, change the bindings and configure the
bridge's allowed hostnames/origins as described in the [bridge reference](execution-bridge.md).
`/api/health` requires authentication; an unsigned 401 is expected.

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

Keep `latest` to receive new releases automatically. A `v1.0.0` tag or digest stays
on that version; for manual upgrades, pin the image and remove `AutoUpdate=registry`.
Choose `alpha`, `beta` or `rc` to follow [prerelease updates](releases.md#alpha-beta-and-release-candidate-channels).
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
