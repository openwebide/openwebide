# Private HTTPS with Tailscale Serve

The recommended HTTPS deployment uses the official Tailscale image as a separate
container alongside Open WebIDE. Serve acts as a reverse proxy: it terminates
HTTPS, manages certificates, and forwards HTTP and WebSocket traffic to the app
and bridge. Phones and other clients connect through Tailscale using a trusted
`https://<device>.<tailnet>.ts.net` address. No custom domain, DNS records or
router port forwarding are needed.

Tailscale remains independently installed and updated. The Open WebIDE image
contains the app and execution bridge; it does not contain Tailscale.

## Prerequisites

- Install Tailscale on each client device and join the same tailnet.
- Enable MagicDNS and HTTPS Certificates in the tailnet's DNS settings.
- Create a non-ephemeral auth key for the server container. Supply it through
  the `TS_AUTHKEY` environment variable at startup; keep it out of committed files.
- Choose the node name `openwebide` and find your tailnet DNS suffix in the
  Tailscale admin console. Substitute the full hostname below. If that name is
  already in use, choose a unique name and update `TS_HOSTNAME` too.

See [Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve) and
[container configuration](https://tailscale.com/docs/features/containers/docker/docker-params)
for authentication, certificate and networking options.

## Docker deployment

Use `docker-compose.https.yml` on its own. It publishes no host ports: the app and
bridge bind to loopback inside Tailscale's network namespace. The app database,
workspace and Tailscale identity remain in separate persistent volumes/mounts.

The file uses the published GHCR image (override it with `OPENWEBIDE_IMAGE`); it
becomes installable after the first public release. Release assets also include
`serve-config.sh` so this setup does not require a checkout. For a source checkout,
build with the override instead:

```sh
docker compose -f docker-compose.https.yml -f docker-compose.https.build.yml up -d --build
```

Use the same two `-f` arguments for subsequent source-build operations.

```sh
export OPENWEBIDE_TAILSCALE_HOST=openwebide.tailNNNN.ts.net
export OPENWEBIDE_SERVE_CONFIG_DIR="$HOME/.config/openwebide/tailscale"
mkdir -p "$OPENWEBIDE_SERVE_CONFIG_DIR"
sh serve-config.sh "$OPENWEBIDE_TAILSCALE_HOST" \
  > "$OPENWEBIDE_SERVE_CONFIG_DIR/serve.json"
# Set TS_AUTHKEY in your shell; do not put it in a committed file.
docker compose -f docker-compose.https.yml up -d
docker compose -f docker-compose.https.yml exec tailscale tailscale status
docker compose -f docker-compose.https.yml exec tailscale tailscale serve status
```

In a checkout, the generator is `docker/tailscale/serve-config.sh` rather than
the downloaded `serve-config.sh`.

Set `OPENWEBIDE_WORKSPACE` if your projects are outside `$HOME/source`. Set
`OPENWEBIDE_TAILSCALE_NAME` if using a node name other than `openwebide`. The full
hostname in `OPENWEBIDE_TAILSCALE_HOST` must match the enrolled node; a conflicting
node name may acquire a suffix. After enrollment, you can remove `TS_AUTHKEY` and
recreate the container with the same state volume. Recreate the app too whenever
recreating Tailscale, so they share the current namespace.

The mounted Serve JSON is declarative and survives container recreation. Serve
strips `/bridge` before forwarding to port 3001; it preserves the browser Origin
and the public Host for the bridge's existing allowlist checks. `/` forwards to
Spin, including REST and streaming responses. It supports WebSocket upgrades at
`/bridge` as well as bridge HTTP requests such as `/bridge/health`. The
backend-only secret bootstrap route is not exposed through the proxy.

Open `https://$OPENWEBIDE_TAILSCALE_HOST`. New browser configurations automatically
use `wss://<same-host>/bridge`. An existing explicit **Settings → Bridge URL** is
retained; change it to this route if migrating from the old port 8443 setup.
**Settings → Install app** offers the browser install prompt when available, or
browser-specific instructions. On iPhone/iPad, use Safari's Share → Add to Home
Screen. HTTP LAN origins show the HTTPS setup guidance; HTTP localhost qualifies
as a secure context.

Serve is private tailnet access; this configuration explicitly disables Funnel.

## Rootless Podman

The same topology works without TUN devices or additional capabilities: run the
official userspace Tailscale container first, then join its network namespace.
Use your existing database/workspace mounts when migrating; never run two app
instances against the same database.

```sh
podman build -t openwebide:local .
podman run -d --name openwebide-tailscale \
  -v openwebide-tailscale:/var/lib/tailscale \
  -v "$OPENWEBIDE_SERVE_CONFIG_DIR:/config:ro,Z" \
  -e TS_AUTHKEY -e TS_AUTH_ONCE=true -e TS_USERSPACE=true \
  -e TS_HOSTNAME=openwebide -e TS_STATE_DIR=/var/lib/tailscale \
  -e TS_SERVE_CONFIG=/config/serve.json tailscale/tailscale:stable
podman run -d --name openwebide \
  --network container:openwebide-tailscale \
  -v openwebide-data:/app/.spin -v "$HOME/source:/workspace:Z" \
  -e OPENWEBIDE_APP_HOST=127.0.0.1 -e OPENWEBIDE_BRIDGE_HOST=127.0.0.1 \
  -e OPENWEBIDE_BRIDGE_ALLOWED_HOSTS="$OPENWEBIDE_TAILSCALE_HOST" \
  -e OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS="https://$OPENWEBIDE_TAILSCALE_HOST" \
  openwebide:local
podman exec openwebide-tailscale tailscale serve status
```

Integrate the containers into systemd/Quadlet for restart management; the
[app guide](podman-quadlet.md) covers data persistence. Keep Tailscale outside the
app image and lifecycle. On SELinux hosts use the bind mount labels shown above.

## Native deployment

Run the native app on port 3000 and bridge on **127.0.0.1:3001**. Before starting
the bridge, set its allowed public hostname and browser origin:

```sh
export OPENWEBIDE_BRIDGE_ALLOWED_HOSTS="$OPENWEBIDE_TAILSCALE_HOST"
export OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS="https://$OPENWEBIDE_TAILSCALE_HOST"
tailscale serve --bg --https=443 http://127.0.0.1:3000
tailscale serve --bg --https=443 --set-path=/bridge http://127.0.0.1:3001
tailscale serve status
```

These background Serve settings persist in Tailscale's state. Both commands use
port 443, yielding the same-origin app and bridge routes. Explicit old Bridge URL
settings still need to be updated as described above.

## Host Tailscale Serve with container deployments (Podman Quadlet or Docker)

If Tailscale is already installed directly on the Linux host (`tailscaled`), you do not need to run a secondary Tailscale container. You can use native `tailscale serve` on the host to terminate HTTPS and reverse-proxy into the containerized Open WebIDE app and execution bridge.

1. Publish the container's app and bridge ports to host loopback (for example, host ports 8080 and 3001, or 8081 and 3001 if port 8080 is used by a local LLM).
2. Configure the container's environment with your Tailscale MagicDNS hostname:
   ```ini
   Environment=OPENWEBIDE_BRIDGE_ALLOWED_HOSTS=<device>.<tailnet>.ts.net
   Environment=OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS=https://<device>.<tailnet>.ts.net
   ```
   *(For Docker Compose, set these under `environment:`).*
3. On the host, configure background Serve rules for both the root app and the `/bridge` endpoint:
   ```sh
   tailscale serve --bg --https=443 http://127.0.0.1:8080
   tailscale serve --bg --https=443 --set-path=/bridge http://127.0.0.1:3001
   tailscale serve status
   ```
   *(Replace `8080` with your mapped app port if different.)*

Tailscale Serve routes incoming HTTPS requests on port 443 to the app and strips `/bridge` when forwarding to port 3001, preserving the public `Host` and `Origin` headers required by the bridge's security checks. Mobile devices and other clients on your tailnet can then open `https://<device>.<tailnet>.ts.net` directly without opening raw ports. When navigating on mobile browsers, specify `https://` explicitly as port 80 is not served by default.

## Alternative: Caddy with an internal CA

Clients must trust Caddy's internal CA. Use your chosen hostname in the bridge
Host/Origin allowlists and replace `webide.home` here:

```caddyfile
webide.home {
    tls internal
    handle /bridge/secret {
        respond "Not available through the proxy" 403
    }
    @bridge path /bridge /bridge/*
    handle @bridge {
        uri strip_prefix /bridge
        reverse_proxy 127.0.0.1:3001
    }
    handle {
        reverse_proxy 127.0.0.1:3000
    }
}
```

Keep app/bridge listeners on loopback. The proxy handles TLS, SSE and WebSockets;
no TLS certificates need to be installed in the bridge. PWA installation depends
on the browser trusting the certificate, not merely the presence of HTTPS.

## Verification

The repeatable transport smoke test uses Caddy's internal CA and the same shared
network-namespace topology. It verifies real TLS, secure session cookies, REST,
SSE and authenticated WebSockets in both workspace modes, checks proxy request
guards, and restarts the app to verify persistence. Run it after building:

```sh
python3 docker/https-test/check.py docker --image openwebide:local
# Use the same image in Podman, then run the rootless contract:
docker save openwebide:local | podman load
python3 docker/https-test/check.py podman --image openwebide:local --port 8447
```

Both engine contracts have passed. Live verification of the official Tailscale
container deployment also passed in Docker, including trusted certificates,
secure cookies, REST file write/read/delete, incremental SSE and authenticated
WSS in both workspace modes. Native Tailscale Serve passed the same transport
checks. Chrome received an install prompt over the trusted tailnet HTTPS origin,
with an active service worker that excludes API and bridge response caching.

Recreating the Tailscale and app containers with their existing volumes preserved
the node ID, hostname, IP addresses, Serve JSON and account database without
re-enrollment. Both-mode transport checks passed again after recreation. The
checks used an isolated node, account, workspace and deterministic model provider.

After deployment, check `/bridge/health`, sign in, and confirm the bridge connects.
Open a remote project, write/read/delete a temporary file, then repeat with a local
folder. Check terminals, approval prompts and a streaming chat response in each
mode. These exercise REST, SSE and WebSocket forwarding and browser file access.
Disable the server/network and reload: the app should show **Can't reach the
server**, without showing cached project or conversation data. Restore the
connection and use Retry.

The service worker caches only versioned shell assets and its offline screen. API
responses and bridge traffic are never cached. Existing open windows retain their
worker until closed; reopen the app after an upgrade to activate the new build.
