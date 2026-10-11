# Releases and upgrades

[Alpha `1.0.0-alpha.6`](https://github.com/openwebide/openwebide/releases/tag/v1.0.0-alpha.6)
is published with Linux amd64/arm64 images, Linux/macOS bridge archives and
versioned installation assets. Successful `main` builds update the rolling `nightly`
image automatically; `preview` and `stable` are published from version tags. GHCR currently requires registry authentication.
The package owner can enable anonymous pulls by making the container package
public in [organization package settings](https://github.com/orgs/openwebide/packages).
The remaining stable-release and installation checks are tracked on the roadmap.

## Install a published release

Download `docker-compose.yml` from
[GitHub Releases](https://github.com/openwebide/openwebide/releases). It uses a
versioned GHCR image and requires no checkout or Rust build. From a dedicated
installation directory:

```sh
mkdir -p ~/source
docker compose pull
docker compose up -d
```

Open <http://localhost:8080/> and register the first account. Set
`OPENWEBIDE_WORKSPACE=/path/to/projects` before starting if your projects are
elsewhere. Configure your model server through **Servers**. From a container,
`localhost` refers to the container; use an address reachable from that container
for a model running on another machine or the host. Models and host language
toolchains are supplied separately.

Use [Tailscale HTTPS](tailscale.md) for access from other devices and local browser
folder permissions. Release assets also include a standalone HTTPS Compose file
and `serve-config.sh`, plus [Podman Quadlet units](podman-quadlet.md).

The Quadlet assets track the release channel (`nightly`, `preview` or `stable`)
and enable registry auto-updates; the Compose assets default to the specific version.
See the automatic-update options below.

The image includes both WASM components and the server-side bridge for remote
projects. The optional standalone bridge archives support Linux/macOS on amd64
and arm64; they do not contain the Spin backend or browser app. Verify downloaded
files against `SHA256SUMS` (`sha256sum -c SHA256SUMS` on Linux, or
`shasum -a 256 -c SHA256SUMS` on macOS, for a directory containing all assets).
Extract the matching archive and run `./openwebide-bridge --help`; its bundled
reference describes host requirements and configuration.

## Configuration

| Setting | Default | Purpose |
| --- | --- | --- |
| `OPENWEBIDE_IMAGE` | `ghcr.io/openwebide/openwebide:stable` in repository install files; pinned in Compose release assets | Override the Compose container image/version. |
| `OPENWEBIDE_WORKSPACE` | `$HOME/source` in Compose | Host projects directory mounted at `/workspace`. |
| `OPENWEBIDE_DATA_VOLUME` | `openwebide-data` | Named app data volume; set to an existing volume name when migrating an older install. |
| `OPENWEBIDE_TAILSCALE_VOLUME` | `openwebide-tailscale-data` | Named Tailscale state volume in HTTPS Compose; preserve an existing identity by selecting its old volume. |
| `OPENWEBIDE_BRIDGE` | `1` | Enable the bundled execution bridge; `0` uses SSE chat without its command/terminal features. |
| `OPENWEBIDE_BRIDGE_ALLOWED_HOSTS` | Empty | Comma-separated hostnames allowed by the bridge; localhost and IP literals already work. |
| `OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS` | Empty | Additional comma-separated browser origins when the app and bridge use different hostnames. |
| `SPIN_VARIABLE_BRIDGE_URL` | `http://127.0.0.1:3001` | Backend bridge URL for a source/native installation. |
| `SPIN_VARIABLE_BRIDGE_SECRET` | Empty | Required shared secret for a backend reaching a non-loopback bridge. |

The bundled entrypoint configures the bridge URL/secret itself. For external bridge
configuration, use the [bridge reference](execution-bridge.md). HTTPS deployment
has its own `OPENWEBIDE_TAILSCALE_*`, `OPENWEBIDE_SERVE_CONFIG_DIR` and `TS_AUTHKEY`
settings described in the [Tailscale guide](tailscale.md). Model URLs, credentials
and runtime profiles are configured in **Servers**; user defaults and preferences
are in **Settings**, persisted in SQLite.

## Upgrade and restore

### Existing installations with the old project name

Deployment names now use `openwebide`. Before replacing an older Compose file,
inspect its mounted volume names with `docker compose config` and
`docker inspect <existing-app-container>`. Stop the old deployment using its old
Compose file, and set `OPENWEBIDE_DATA_VOLUME` in the new installation's `.env` to
the existing app data volume name (often `open-webide_openwebide-data`). For HTTPS,
set `OPENWEBIDE_TAILSCALE_VOLUME` to its existing Tailscale state volume too. Retain
the enrolled node name via `OPENWEBIDE_TAILSCALE_NAME` and its full hostname via
`OPENWEBIDE_TAILSCALE_HOST`. This reuses the account database, bridge secret and
Tailscale identity instead of creating fresh volumes.

For older Quadlet installs, stop the old service and copy its data directory into
`~/.local/state/openwebide/data/` before installing the renamed units, or keep the
old data path in the new unit's `Volume=` line. Remove the old unit files and reload
systemd so only the renamed service starts. Keep the old data/volume until the new
installation has been verified; do not delete volumes as part of renaming.

### Routine upgrades

Compose uses the explicit `openwebide` project name and stable data volume name.
Preserve any configured volume overrides on every upgrade. Read the target release's
changelog first. Stop the app and back up its entire data directory before pulling:

```sh
docker compose stop
mkdir -p backups
docker compose run -T --rm --no-deps --entrypoint tar openwebide \
  -czf - -C /app .spin > "backups/openwebide-$(date +%Y%m%d-%H%M%S).tar.gz"
```

Choose a unique backup filename for each upgrade. This archive contains accounts,
sessions, settings and the bridge secret. Back up your project files independently,
including `.openwebide/backups/` while pending agent changes need restoration.
For native installs, stop Spin and the bridge and copy the entire `.spin/` directory.
For Quadlet, stop the service and copy `~/.local/state/openwebide/data/`.

Set `OPENWEBIDE_IMAGE=ghcr.io/openwebide/openwebide:v<version>` in the installation's
`.env` file, then run `docker compose pull && docker compose up -d`. Quadlet users
change `Image=` in `openwebide.image`, reload systemd, and restart
`openwebide-image.service` followed by `openwebide.service`.

SQLite migrations run automatically once per schema version. An older build
refuses to open a newer database; downgrading the image alone is not a rollback.
To roll back, stop the app, restore the full pre-upgrade data directory and the
previous image version, then restart. Restore project files separately if a run
changed them. Do not use `docker compose down -v` during upgrades: it deletes data.

After upgrading, sign in and verify an existing session, settings, a streaming
response and file access in both workspace modes. Close all app windows and reopen
to activate the new PWA shell; local folders may need **Grant folder access** again.

## Automatic updates

Podman units include `Pull=newer`, `AutoUpdate=registry` and a rolling channel
image (`stable` for stable releases). Enable `podman-auto-update.timer` as shown
in the [Quadlet guide](podman-quadlet.md#upgrade).

Docker Compose has no built-in background update service. Its equivalent is a
scheduled job that runs `docker compose pull` followed by `docker compose up -d`
in the existing installation directory. The [pull command](https://docs.docker.com/reference/cli/docker/compose/pull/)
downloads images; [up](https://docs.docker.com/reference/cli/docker/compose/up/)
recreates services whose images changed. `restart: unless-stopped` alone does not
check for new images.

Set `OPENWEBIDE_IMAGE=ghcr.io/openwebide/openwebide:stable` in your installation's
`.env` file to follow releases. On Linux, this example user service and timer
check daily; replace the working directory and Docker binary path as needed.
The user running the service must have access to the Docker daemon.

```ini
# ~/.config/systemd/user/openwebide-update.service
[Unit]
Description=Update Open WebIDE containers

[Service]
Type=oneshot
WorkingDirectory=%h/openwebide
ExecStart=/usr/bin/docker compose pull
ExecStart=/usr/bin/docker compose up -d
TimeoutStartSec=900
```

```ini
# ~/.config/systemd/user/openwebide-update.timer
[Unit]
Description=Check for Open WebIDE updates daily

[Timer]
OnCalendar=daily
Persistent=true
RandomizedDelaySec=30m

[Install]
WantedBy=timers.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now openwebide-update.timer
```

For HTTPS Compose, include `-f docker-compose.https.yml` in both `ExecStart`
commands. On other hosts, schedule those same commands with the platform's task
scheduler. Keep regular backups: automatic updates restart the app and apply
database migrations. Use a versioned tag and disable the update timer when you
want to control upgrade timing.

## Nightly, preview and stable channels

| Source | Rolling image tag | Pinned image tag | GitHub Release |
| --- | --- | --- | --- |
| Successful `main` build | `nightly` | `sha-<full-commit-sha>` | None |
| `v1.0.0-rc.1` tag | `preview` | `v1.0.0-rc.1` | Prerelease |
| `v1.0.0` tag | `stable` | `v1.0.0` | Stable |

Every push to `main` starts the Release workflow, including the shared CI checks
and native amd64/arm64 image smoke tests. It publishes the tested image to `nightly`
and a commit-specific tag. New pushes supersede unfinished main builds. Failed
checks leave the published channel unchanged. About displays the build commit;
rolling builds retain the workspace version until a tagged release is prepared.

Main builds publish container images, which include the server bridge. Tagged
preview/stable releases also publish standalone Linux/macOS bridge archives,
installation files and checksums. Existing numbered alpha releases remain pinned
snapshots; future nightly updates need no version bump, changelog rollover or tag.
Preview never updates `stable` or GitHub's latest release. Publish version tags
sequentially so an older release cannot finish later and replace a newer channel.

For Compose, set `OPENWEBIDE_IMAGE=ghcr.io/openwebide/openwebide:nightly` in `.env`
to follow main, or use `:sha-<full-commit-sha>` to pin a build. For Podman, set the
same image in `openwebide.image`; the auto-update timer follows the chosen channel.
Downloaded Quadlet assets select `preview` or `stable`; Compose downloads pin
the release version. Repository install files default to `stable`.
Historical alpha Quadlet downloads need their image changed to `nightly` to follow
main. There are no `alpha`, `latest`, `rc` or `beta` aliases. A channel exists after its first publication.

Rolling nightly publication does not complete the stable 1.0 release gates.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Image cannot be pulled | The release exists and the GHCR package is public; unpublished versions cannot be installed. |
| App is unavailable | `docker compose ps`, `docker compose logs --tail=100`, port 8080 and volume permissions. |
| Health API returns 401 | `/api/health` requires sign-in; this is expected for an unsigned request. |
| Model connection fails | Container-reachable model URL, authentication and model-server logs. |
| Bridge disconnected | `/health` on bridge port 3001, hostname/origin allowlists, or `/bridge/health` through HTTPS. |
| Local command/Git tools missing | The optional bridge must be on the browser's device and see the selected folder; browser-only local editing does not require it. |
| Remote edits do not reach host files | Workspace mount and Spin's `--direct-mounts --allow-transient-write` flags; source installs need normal host access. |
| Local folder cannot reopen | Chromium browser, secure HTTPS/localhost origin, and **Grant folder access**. |

Use the [support issue form](https://github.com/openwebide/openwebide/issues/new/choose)
with your version, deployment method, browser and workspace mode.

## Maintainer release procedure

PRs run CI directly; main and tag builds run the same CI workflow through Release,
so main pushes run the checks once. The following steps are for preview/stable
release snapshots.

1. For stable 1.0, finish the release gates in the [roadmap](roadmap.md), including both-mode
   verification and hands-on device/environment checks. The prepared pipeline alone
   does not complete the 1.0 milestone.
2. Run the **Release** workflow manually on the intended commit. This runs CI,
   builds/smoke-tests images on native Linux amd64/arm64 runners and packages native
   Linux/macOS bridges; it uploads artifacts without publishing GHCR or a release.
   Validate downloaded archives and try the install assets with a locally loaded
   matching image via `OPENWEBIDE_IMAGE`. Exercise Compose and rootless Quadlet,
   persistence, upgrades, both workspace modes and real-model streaming.
3. Set the same version in `Cargo.toml` and `spin.toml`, update `Cargo.lock`, and
   move shipped changelog entries into its versioned section, leaving Unreleased
   for subsequent work. Use `MAJOR.MINOR.PATCH-rc.N` for preview (numeric `N`, no
   leading zeros), or `MAJOR.MINOR.PATCH` for stable. Rolling nightly only needs a
   push to `main`; keep its changes under Unreleased. Update the README/site/guide to describe the actual
   release status. Run `python3 tools/release.py validate --tag v<version>`.
4. Commit the reviewed release changes and push the corresponding `v<version>` tag.
   Tag builds require matching manifest/lock versions and nonempty tagged changelog
   notes, then run all CI checks before packaging or publication. The workflow
   pushes native platform images, merges their digests into the versioned tag, attaches
   archives/install files/checksums to a draft GitHub Release, publishes it and
   updates the appropriate channel. Preview releases carry GitHub's prerelease
   flag and leave the `stable` image and GitHub latest release untouched.
5. On the first publication, set the GHCR package to **Public** in package settings
   and check it is linked to this repository (the image carries the source label).
   Verify an anonymous pull for both architectures and the versioned release install.
   A first package may be private until this setting is changed; announce it only
   after anonymous installation works. See [GitHub package visibility](https://docs.github.com/en/packages/learn-github-packages/configuring-a-packages-access-control-and-visibility).

The release workflow needs only the repository's `GITHUB_TOKEN` with
`contents: write` and `packages: write` in publishing jobs. No external registry,
signing account or persistent publishing secret is required. macOS archives are
unsigned; verify their checksums and assess signing/notarization if distribution
requirements change. See [native multi-platform image builds](https://docs.docker.com/build/ci/github-actions/multi-platform/)
and [draft-first GitHub Releases](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository).

## Git SSH credentials

Release assets include the optional `docker-compose.ssh.yml` overlay. Use it with
the downloaded Compose file to forward a host agent and public SSH configuration;
see [Git authentication](git-ssh.md). Quadlet includes matching opt-in examples.
