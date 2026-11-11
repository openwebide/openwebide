"""Deterministic LSP peer for service startup/close lifecycle contracts."""
import json
from pathlib import Path
import sys
import time

root = Path(__file__).parent
configuration = (root / "settings.json").read_text() if (root / "settings.json").exists() else "retained service"
while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line == b"\r\n":
            break
        name, value = line.decode().split(":", 1)
        headers[name.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    with (root / "calls").open("a") as calls:
        calls.write(json.dumps(message) + "\n")
    method = message.get("method")
    result = None
    if method == "initialize":
        while (root / "wait").exists() and not (root / "release").exists():
            time.sleep(0.01)
        result = {"capabilities": {"textDocumentSync": 1, "hoverProvider": True}}
    elif method == "textDocument/hover":
        if (root / "hold-hover").exists():
            (root / "hover-started").touch()
            while not (root / "release-hover").exists():
                time.sleep(0.01)
        result = {"contents": configuration}
    if "id" in message:
        body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}).encode()
        sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        sys.stdout.buffer.flush()
