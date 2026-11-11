"""LSP peer using a real persistent cache selected through declared bindings."""
import json
import os
from pathlib import Path
import sys
import subprocess

if "--version" in sys.argv:
    print(Path("server.version").read_text() if Path("server.version").exists() else "cache-peer-v1")
    sys.exit(0)

cache = Path(os.environ["FIXTURE_CACHE"])
cache.mkdir(parents=True, exist_ok=True)
artifact = cache / "derived.txt"
hit = artifact.exists()
if not hit:
    artifact.write_text("derived-on-first-start")
opened = {}
options = None
settings = None
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
    result = None
    if method == "initialize":
        options = params.get("initializationOptions")
        result = {"capabilities": {"textDocumentSync": 1, "hoverProvider": True}}
    elif method == "workspace/didChangeConfiguration":
        settings = params["settings"]
        growth = settings.get("growthBytes", 0)
        if 0 < growth <= 4 * 1024 * 1024:
            # A bounded background indexer for native process-tree retirement
            # tests. It waits for the controller before exceeding its budget.
            subprocess.Popen(
                [sys.executable, "-c", """import pathlib,sys,time
cache=pathlib.Path(sys.argv[1]);size=int(sys.argv[2])
while not (cache/'grow.trigger').exists():time.sleep(.01)
while True:
    (cache/'grown').write_bytes(b'x'*size)
    time.sleep(.05)
""", str(cache), str(growth)],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        document = params["textDocument"]
        opened[document["uri"]] = document.get("text", params.get("contentChanges", [{}])[0].get("text"))
    elif method == "textDocument/didClose":
        opened.pop(params["textDocument"]["uri"], None)
    elif method == "textDocument/hover":
        result = {"contents": json.dumps({"cache": str(cache), "hit": hit, "options": options, "settings": settings, "sources": list(opened.values())})}
    if "id" in message:
        body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}).encode()
        sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        sys.stdout.buffer.flush()
