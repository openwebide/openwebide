"""Check file-owner continuity across fresh default-policy Linux containers.

Usage: python3 docker/native-recovery/check-owners.py IMAGE MATCHING_LIBTEST OUTPUT
Retains containers and both owned volumes; not full-app or compound-command proof.
"""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import time
import uuid

image, executable, destination = sys.argv[1:]
executable = Path(executable).resolve(strict=True)
output = Path(destination).resolve()
output.mkdir(parents=True, exist_ok=False)
prefix = "openwebide-owner-" + uuid.uuid4().hex[:12]
rows = []


def run(name, args):
    started = time.monotonic()
    result = subprocess.run(args, capture_output=True, timeout=600)
    (output / (name + ".stdout")).write_bytes(result.stdout)
    (output / (name + ".stderr")).write_bytes(result.stderr)
    rows.append({"case": name, "command": args, "returncode": result.returncode,
                 "seconds": round(time.monotonic() - started, 2),
                 "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
                 "stderr_sha256": hashlib.sha256(result.stderr).hexdigest()})
    (output / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    assert result.returncode == 0, name
    return result.stdout.decode()


image = run("image", ["docker", "image", "inspect", image, "--format", "{{.Id}}" ]).strip()
data, workspace = prefix + "-data", prefix + "-workspace"
for volume in [data, workspace]:
    run(volume, ["docker", "volume", "create", "--label", "org.openwebide.owner-check=" + prefix, volume])
original = hashlib.sha256(executable.read_bytes()).hexdigest()
base = ["docker", "run", "--network", "none", "--read-only",
        "--volume", data + ":/app/.spin", "--volume", workspace + ":/workspace"]
complete = False
try:
    folders = []
    for stage in ["initialize", "ordinary", "recover", "ordinary", "resolve"]:
        name = str(len(folders)) + "-" + stage
        text = run(name, base + ["--name", prefix + "-" + name,
            "--volume", str(executable) + ":/recovery-test:ro",
            "--env", "OPENWEBIDE_TEST_PACKAGED_OWNER_STAGE=" + stage,
            "--env", "HOME=/profile-" + name, "--env", "XDG_STATE_HOME=/different-" + name,
            "--entrypoint", "/recovery-test", image, "--exact",
            "native_recovery::owners::tests::packaged_owner_survives_an_outer_runtime_recreation", "--nocapture"])
        assert "packaged_owner_stage=" + stage in text
        assert "test result: ok. 1 passed; 0 failed; 0 ignored;" in text
        folder = json.loads(next(line.removeprefix("packaged_owner_folder=") for line in text.splitlines() if line.startswith("packaged_owner_folder=")))
        folders.append(folder)
    assert all(folder["root"] == folders[0]["root"] and folder["path"] == folders[0]["path"] for folder in folders)
    assert any(folder["ancestors"] != folders[0]["ancestors"] for folder in folders[1:]), "Runtime ancestry must actually change"
    assert hashlib.sha256(executable.read_bytes()).hexdigest() == original
    complete = True
finally:
    (output / "terminal.json").write_text(json.dumps({"complete": complete, "image": image,
        "executable": str(executable), "sha256": original, "data_volume": data,
        "workspace_volume": workspace, "containers_and_volumes_retained": True,
        "original_file_owner_only": True, "compound_commands_complete": False,
        "cases": rows}, indent=2) + "\n")
