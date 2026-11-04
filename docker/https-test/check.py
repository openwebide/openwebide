"""Real TLS, REST, SSE and WSS checks; all accounts/data belong to disposable containers."""
import argparse
import base64
import http.cookiejar
import json
import os
from pathlib import Path
import socket
import ssl
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("engine", choices=["docker", "podman"])
parser.add_argument("--image", default="openwebide:ci")
parser.add_argument("--port", type=int, default=8446)
args = parser.parse_args()
engine = args.engine
prefix = "webide-https-" + uuid.uuid4().hex[:8]
containers = [prefix + suffix for suffix in ["-model", "-app", "-proxy"]]
data_volume = prefix + "-data"
source = Path(__file__).resolve().parent

def cli(*command, check=True):
    return subprocess.run([engine, *command], check=check, capture_output=True, text=True)

def exact(stream, count):
    data = b""
    while len(data) < count:
        part = stream.recv(count - len(data))
        if not part:
            raise AssertionError("WebSocket disconnected")
        data += part
    return data

def ws_send(stream, message):
    data = json.dumps(message).encode()
    mask = os.urandom(4)
    header = bytes([0x81, 0x80 | len(data)]) if len(data) < 126 else bytes([0x81, 0x80 | 126]) + struct.pack("!H", len(data))
    stream.sendall(header + mask + bytes(byte ^ mask[i % 4] for i, byte in enumerate(data)))

def ws_receive(stream):
    first, second = exact(stream, 2)
    length = second & 127
    if length == 126:
        length = struct.unpack("!H", exact(stream, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", exact(stream, 8))[0]
    assert first & 15 == 1, "Expected a JSON text frame"
    return json.loads(exact(stream, length))

try:
    if engine == "podman":
        assert cli("info", "--format", "{{.Host.Security.Rootless}}").stdout.strip() == "true", "Podman test must be rootless"
    with tempfile.TemporaryDirectory(prefix="webide-https-") as directory:
        root = Path(directory)
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / "sample").mkdir()
        (workspace / "sample" / "demo.txt").write_text("initial\n")
        # Container-owned logs/database stay in an engine volume, avoiding
        # root-owned files in the host's temporary directory on Linux Docker.
        cli("volume", "create", data_volume)
        model, app, proxy = containers
        origin = f"https://localhost:{args.port}"
        cli("run", "-d", "--name", proxy, "-p", f"127.0.0.1:{args.port}:3443", "-v", f"{source / 'Caddyfile'}:/etc/caddy/Caddyfile:ro", "caddy:2")
        cli("run", "-d", "--name", app, "--network", f"container:{proxy}", "-v", f"{workspace}:/workspace", "-v", f"{data_volume}:/app/.spin",
            "-e", "OPENWEBIDE_APP_HOST=127.0.0.1", "-e", "OPENWEBIDE_BRIDGE_HOST=127.0.0.1", "-e", f"OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS={origin}", args.image)
        cli("run", "-d", "--name", model, "--network", f"container:{proxy}", "-v", f"{source / 'model.py'}:/model.py:ro", "python:3-alpine", "python", "/model.py")
        for _ in range(100):
            ready = cli("exec", model, "python", "-c", "import json, urllib.request; assert json.load(urllib.request.urlopen('http://127.0.0.1:5005/v1/models', timeout=1))['data'][0]['id'] == 'proxy-test'", check=False)
            if ready.returncode == 0:
                break
            time.sleep(0.1)
        else:
            raise AssertionError("Model fixture did not become ready: " + ready.stderr)
        ca = root / "ca.crt"
        for _ in range(200):
            result = cli("cp", f"{proxy}:/data/caddy/pki/authorities/local/root.crt", str(ca), check=False)
            if result.returncode == 0:
                break
            time.sleep(0.1)
        assert ca.exists(), "Proxy did not create its CA"
        context = ssl.create_default_context(cafile=str(ca))
        jar = http.cookiejar.CookieJar()
        client = urllib.request.build_opener(urllib.request.HTTPSHandler(context=context), urllib.request.HTTPCookieProcessor(jar))
        def request(path, body=None, method=None, headers=None, *, timeout=30):
            payload = None if body is None else body if isinstance(body, bytes) else json.dumps(body).encode()
            return client.open(urllib.request.Request(origin + path, data=payload, method=method,
                headers={"Content-Type": "application/json", "x-openwebide": "1", **(headers or {})}), timeout=timeout)
        def api(path, body=None, method=None, *, timeout=30):
            with request("/api" + path, body, method, timeout=timeout) as response:
                return json.load(response)
        for _ in range(200):
            try:
                with request("/bridge/health") as response:
                    assert response.status == 200
                with request("/") as response:
                    assert response.status == 200
                break
            except (OSError, urllib.error.HTTPError):
                time.sleep(0.1)
        else:
            raise AssertionError("App and bridge did not become ready through HTTPS")
        api("/auth/register", {"username": "https-check", "password": "disposable-https-test-password"})
        assert jar and all(cookie.secure for cookie in jar), "TLS proxy must produce Secure session cookies"
        # Prepare the complete default package baseline before transport checks,
        # using the normal installation API and its cold provisioning budget.
        baseline = json.loads((source.parent.parent / "plugins/bundled.json").read_text())
        installed = api("/plugins", timeout=300)
        assert {
            (entry["prepared"]["source"]["repository"], entry["prepared"]["source"]["commit"], entry["prepared"]["source"]["path"])
            for entry in installed
        } == {(baseline["repository"], baseline["commit"], path) for path in baseline["paths"]}, "Default packages did not initialize"
        # This fixture verifies transport, including unbuffered SSE. Give the
        # registered tools room so compaction cannot preempt the first delta.
        connection = api("/connections", {"name": "HTTPS fixture", "kind": "llamacpp", "base_url": "http://127.0.0.1:5005", "model": "proxy-test", "context_limit": 32768})
        token = api("/bridge/token", {})["token"]
        for route, headers, status in [("/bridge/secret", {}, 403), ("/bridge/host/info", {}, 401), ("/bridge/health", {"Origin": "https://evil.invalid"}, 403)]:
            try:
                request(route, {} if route.endswith("secret") else None, headers=headers)
                raise AssertionError(f"{route} should reject this request")
            except urllib.error.HTTPError as error:
                assert error.code == status, (route, error.code)
        # A legitimate short-lived browser token retains host-info and shell access.
        with request("/bridge/host/info", headers={"Origin": origin, "Authorization": "Bearer " + token}) as response:
            assert response.status == 200
        for mode in ["local", "remote"]:
            project = api("/projects", {"name": mode, "mode": mode, "path": "sample" if mode == "remote" else None})
            session = api("/sessions", {"name": mode, "auto_title": False, "connection_id": connection["id"], "system_prompt_id": None, "project_id": project["id"]})
            if mode == "remote":
                path = f"/api/projects/{project['id']}/files/"
                with request(path + "write?path=probe.txt", b"HTTPS write", "PUT") as response:
                    assert response.status == 200
                with request(path + "read?path=probe.txt") as response:
                    assert json.load(response)["content"] == "HTTPS write"
                request(path + "delete?path=probe.txt", method="DELETE").close()
            with request(f"/api/sessions/{session['id']}/messages", {"content": "Transport check"}) as response:
                assert response.headers.get_content_type() == "text/event-stream"
                # The provider holds its second delta until we receive the first.
                # This proves incremental forwarding rather than a buffered response.
                prefix_events = []
                while True:
                    line = response.readline().decode()
                    assert line, "SSE ended before its first delta: " + "".join(prefix_events)
                    prefix_events.append(line)
                    if line.startswith("data: "):
                        event = json.loads(line[6:])
                        if event.get("kind") == "delta" and event.get("content") == "HTTPS ":
                            break
                cli("exec", model, "python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:5005/release', data=b'').close()")
                events = "".join(prefix_events) + response.read().decode()
                assert "stream works" in events, events
            with context.wrap_socket(socket.create_connection(("localhost", args.port), timeout=30), server_hostname="localhost") as stream:
                key = base64.b64encode(os.urandom(16)).decode()
                stream.sendall((f"GET /bridge HTTP/1.1\r\nHost: localhost:{args.port}\r\nOrigin: {origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
                handshake = b""
                while not handshake.endswith(b"\r\n\r\n"):
                    handshake += exact(stream, 1)
                assert b"101 Switching Protocols" in handshake
                ws_send(stream, {"type": "hello", "token": token})
                assert ws_receive(stream)["type"] == "hello_ok"
                ws_send(stream, {"type": "spawn", "id": mode, "command": "sh", "args": ["-c", "printf 'WSS works'"], "cwd": "sample", "env": {}, "pty": False, "cols": 80, "rows": 24})
                output = ""
                for _ in range(20):
                    event = ws_receive(stream)
                    if event["type"] == "output":
                        output += event["data"]
                    if event["type"] == "exited":
                        break
                assert "WSS works" in output, output
        # Volume-backed account state survives an app restart.
        cli("restart", app)
        for _ in range(200):
            try:
                assert api("/auth/me")["user"]["username"] == "https-check"
                break
            except (OSError, urllib.error.HTTPError):
                time.sleep(0.1)
        else:
            raise AssertionError("Persistent account did not survive restart")
        print(f"{engine}: TLS, secure cookies, REST, SSE, WSS, guards and persistence passed in both modes")
except Exception:
    for container in containers:
        result = cli("logs", container, check=False)
        print(f"{container} logs:\n{result.stdout}\n{result.stderr}")
    result = cli("exec", containers[1], "cat", "/app/.spin/logs/backend_stderr.txt", check=False)
    print(f"Backend logs:\n{result.stdout}\n{result.stderr}")
    raise
finally:
    for container in containers:
        cli("rm", "-f", "-v", container, check=False)
    cli("volume", "rm", data_volume, check=False)
