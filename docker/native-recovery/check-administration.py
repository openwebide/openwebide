"""Exercise the normal Linux recovery CLI across fresh packaged containers.

Usage: python3 docker/native-recovery/check-administration.py IMAGE LIBTEST BRIDGE OUTPUT
The native file-owner fixture has no backend. This checks OS policy, retirement,
late-actor fencing and ordinary HTTP writing, not deleted-account application or
received-command recovery. Containers, volumes and logs are retained.
"""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

image, libtest, bridge, destination = sys.argv[1:]
libtest, bridge = Path(libtest).resolve(strict=True), Path(bridge).resolve(strict=True)
output = Path(destination).resolve()
output.mkdir(parents=True, exist_ok=False)
prefix = "openwebide-admin-" + uuid.uuid4().hex[:12]
rows, active = [], []
sha = lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest()
original = {str(p): sha(p) for p in [libtest, bridge]}


def run(name, args, expected=0):
    start = time.monotonic()
    result = subprocess.run(args, capture_output=True, timeout=120)
    stdout, stderr = output / (name + ".stdout"), output / (name + ".stderr")
    stdout.write_bytes(result.stdout)
    stderr.write_bytes(result.stderr)
    rows.append({"case": name, "command": args, "returncode": result.returncode,
                 "expected_returncode": expected, "seconds": round(time.monotonic() - start, 2),
                 "stdout_sha256": sha(stdout), "stderr_sha256": sha(stderr)})
    (output / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    assert result.returncode == expected, (name, result.returncode, expected)
    return result.stdout.decode(), result.stderr.decode()


image = run("image", ["docker", "image", "inspect", image, "--format", "{{.Id}}"])[0].strip()
data, workspace = prefix + "-data", prefix + "-workspace"
for volume in [data, workspace]:
    run(volume, ["docker", "volume", "create", "--label", "org.openwebide.admin-check=" + prefix, volume])
base = ["docker", "run", "--read-only", "--volume", data + ":/app/.spin",
        "--volume", workspace + ":/workspace", "--env", "TMPDIR=/workspace/test-tmp",
        "--volume", str(bridge) + ":/usr/local/bin/openwebide-bridge:ro"]
fixture = "native_recovery::owners::tests::packaged_owner_survives_an_outer_runtime_recreation"


def stage(name, value, expected=0):
    return run(name, base + ["--network", "none", "--name", prefix + "-" + name,
        "--volume", str(libtest) + ":/recovery-test:ro",
        "--env", "OPENWEBIDE_TEST_PACKAGED_OWNER_STAGE=" + value,
        "--entrypoint", "/recovery-test", image, "--exact", fixture, "--nocapture"], expected)


def cli(name, *args, expected=0):
    return run(name, base + ["--network", "none", "--name", prefix + "-" + name,
        "--entrypoint", "/usr/local/bin/openwebide-bridge", image, "recovery", *args], expected)


complete = False
try:
    run("scratch", base + ["--network", "none", "--entrypoint", "/bin/mkdir", image,
                           "-p", "/workspace/test-tmp"])
    cli("absent-namespace", "inspect", expected=1)
    folders = []
    for name, value in [("initialize", "initialize"), ("before-writer", "ordinary"), ("before-recover", "recover")]:
        text, _ = stage(name, value)
        assert "test result: ok. 1 passed; 0 failed;" in text
        folders.append(json.loads(next(line.removeprefix("packaged_owner_folder=")
            for line in text.splitlines() if line.startswith("packaged_owner_folder="))))
    assert all(f["root"] == folders[0]["root"] for f in folders)
    assert any(f["ancestors"] != folders[0]["ancestors"] for f in folders[1:])
    before = json.loads(cli("inspect-original", "inspect")[0])
    assert len(before["owners"]) == 1 and not before["settlements"] and not before["retired"]
    assert "A" * 43 not in json.dumps(before)
    entry = before["owners"][0]
    altered = ("1" if entry["expected"][0] == "0" else "0") + entry["expected"][1:]
    note = "Reviewed the native file effects; original issuer decommissioned"
    cli("stale-review", "retire", entry["id"], "--expected", altered, "--note", note, expected=1)
    assert json.loads(cli("after-stale", "inspect")[0]) == before
    audit = json.loads(cli("retire-original", "retire", entry["id"], "--expected", entry["expected"], "--note", note)[0])
    assert audit["original"] == entry and audit["note"] == note
    assert json.loads(cli("retire-again", "retire", entry["id"], "--expected", entry["expected"], "--note", note)[0]) == audit
    _, denied = stage("late-original", "recover", expected=101)
    assert "This original workspace operation was retired by its OS owner; start a new transaction" in denied
    after = json.loads(cli("inspect-retired", "inspect")[0])
    assert after == {"owners": [], "settlements": [], "retired": [audit]}

    server = prefix + "-ordinary-host"
    secret = "fixture-admin-http-secret-00000000"
    run("start-ordinary-host", base + ["--detach", "--name", server, "--publish", "127.0.0.1::3001",
        "--env", "OPENWEBIDE_BRIDGE_SECRET=" + secret,
        "--entrypoint", "/usr/local/bin/openwebide-bridge", image,
        "--host", "0.0.0.0", "--port", "3001", "--workspace", "/workspace",
        "--backend-url", "http://127.0.0.1:1/api"])
    active.append(server)
    binding = run("ordinary-port", ["docker", "port", server, "3001/tcp"])[0].strip()
    assert binding.startswith("127.0.0.1:")
    url = "http://" + binding
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + 20
    while True:
        try:
            with client.open(url + "/health", timeout=2) as response:
                assert response.status == 200
            break
        except (OSError, urllib.error.URLError):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.1)
    body = json.dumps({"command": "printf verified > after-retirement.txt", "cwd": "/workspace/project", "timeout_seconds": 5}).encode()
    request = urllib.request.Request(url + "/exec", data=body, headers={
        "Authorization": "Bearer " + secret, "Content-Type": "application/json"})
    with client.open(request, timeout=15) as response:
        assert response.status == 200
        reply = response.read()
        (output / "ordinary-exec.json").write_bytes(reply)
        assert json.loads(reply)["exit_code"] == 0
    assert run("ordinary-file", ["docker", "exec", server, "/bin/cat", "/workspace/project/after-retirement.txt"])[0] == "verified"
    assert json.loads(cli("audit-after-writing", "inspect")[0]) == after
    run("stop-ordinary-host", ["docker", "stop", "--time", "20", server])
    active.remove(server)
    run("ordinary-host-log", ["docker", "logs", server])
    assert all(sha(p) == digest for p, digest in original.items())
    complete = True
finally:
    cleanup_errors = []
    for server in active:
        try:
            run("stop-after-failure", ["docker", "stop", "--time", "20", server])
            run("failed-host-log", ["docker", "logs", server])
        except Exception as error:
            cleanup_errors.append(repr(error))
    complete = complete and not cleanup_errors
    result = {"terminal": True, "returncode": 0 if complete else 1, "image": image,
        "binaries": original, "data_volume": data, "workspace_volume": workspace,
        "containers_and_volumes_retained": True, "native_file_owner_CLI_only": True,
        "real_deleted_account_project_application_flow": False,
        "dispatched_command_administration_verified": False,
        "cleanup_errors": cleanup_errors,
        "whole_goal_complete": False, "cases": rows}
    (output / "terminal.json").write_text(json.dumps(result, indent=2) + "\n")
