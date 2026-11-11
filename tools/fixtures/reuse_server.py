"""LSP peer reporting its actual process and complete live overlay inventory."""
import json
import os
from pathlib import Path
import sys
import time
from urllib.parse import unquote, urlparse

opened = {}
versions = {}
root = Path.cwd()
allocation = None
if (root / "allocate-memory").exists():
    allocation = bytearray(96 * 1024 * 1024)
    allocation[::4096] = b"x" * (len(allocation) // 4096)


def send(value):
    body = json.dumps({"jsonrpc": "2.0", **value}).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


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
    method = message.get("method")
    params = message.get("params") or {}
    with (root / "calls").open("a") as calls:
        calls.write(json.dumps({"pid": os.getpid(), "method": method, "params": params}) + "\n")
    result = None
    if method == "initialize":
        if (root / "hold-initialize").exists():
            (root / "initialize-started").touch()
            while not (root / "release-initialize").exists():
                time.sleep(0.01)
        result = {"capabilities": {"textDocumentSync": 1, "hoverProvider": True}}
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        document = params["textDocument"]
        uri = document["uri"]
        opened[uri] = document.get("text", params.get("contentChanges", [{}])[0].get("text"))
        versions[uri] = document["version"]
    elif method == "textDocument/didClose":
        opened.pop(params["textDocument"]["uri"], None)
        versions.pop(params["textDocument"]["uri"], None)
    elif method == "textDocument/hover":
        if opened[params["textDocument"]["uri"]] == "hold A":
            (root / "hover-started").touch()
            while not (root / "release-hover").exists():
                time.sleep(0.01)
        def name(uri):
            return Path(unquote(urlparse(uri).path)).name
        result = {"contents": json.dumps({
            "pid": os.getpid(),
            "open": {name(uri): text for uri, text in opened.items()},
            "versions": {name(uri): version for uri, version in versions.items()},
        })}
    if "id" in message:
        send({"id": message["id"], "result": result})
